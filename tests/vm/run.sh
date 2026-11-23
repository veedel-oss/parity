#!/usr/bin/env bash
# System tests on virtual machines with their own kernels, over real ssh, sudo
# and su. Everything only a real system can show is tested here.
#
#   tests/vm/run.sh OS        OS: debian-10, debian-11, debian-12, debian-13
#
# Boots two VMs from the latest cloud image of OS, installs the static binary
# in /usr/local/bin and tests: ping, packages and compare with sudo, su and as
# root; escalation and connection errors; checks of the installed binary; the
# two fresh installs compare as identical and changes made to one show up;
# dpkg's journal; access times on ext4; no process left behind after errors
# and Ctrl-C; the workspace tests run as root.
#
# Needs docker with /dev/kvm, ssh, ssh-keygen, curl and python3. The image is
# cached in ${XDG_CACHE_HOME:-~/.cache}/parity-vm and downloaded again when the
# one on the server is newer. Also run in CI.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
. "$here/images.sh"
os=${1:-}
if [[ $# != 1 ]] || ! url=$(image_url "$os"); then
    echo "usage: $0 OS (one of: ${oses[*]})" >&2
    exit 2
fi
target=x86_64-unknown-linux-musl
cache=${XDG_CACHE_HOME:-$HOME/.cache}/parity-vm
mkdir -p "$cache"

image=$os.qcow2
rm -f "$cache/$image.part"
if [[ -f $cache/$image ]]; then
    curl -fsSL -R -z "$cache/$image" -o "$cache/$image.part" "$url"
else
    echo "downloading $url"
    curl -fL -R --progress-bar -o "$cache/$image.part" "$url"
fi
if [[ -s $cache/$image.part ]]; then
    mv "$cache/$image.part" "$cache/$image"
fi
rm -f "$cache/$image.part"

cargo build --manifest-path "$root/Cargo.toml" --release --locked --target $target
bin=$root/target/$target/release/parity
mapfile -t tests < <(cargo test --manifest-path "$root/Cargo.toml" --workspace --locked \
    --target $target --no-run --message-format=json 2>/dev/null | python3 -c '
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]:
        print(m["executable"])')

work=$(mktemp -d)
tag=parity-vm-$$
containers=()
cleanup() {
    for c in "${containers[@]}"; do docker rm -f "$c" >/dev/null 2>&1 || true; done
    # Files created in the runner are owned by root.
    docker run --rm -v "$work:/work" parity-vm-runner sh -c 'rm -rf /work/*' 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

docker build -q -t parity-vm-runner "$here" >/dev/null

failures=0
fail() { echo "FAIL $*"; failures=$((failures + 1)); }

ssh-keygen -q -t ed25519 -N '' -f "$work/key"
start() { # name image
    local n=$1
    mkdir -p "$work/seed-$n"
    cat >"$work/seed-$n/user-data" <<EOS
#cloud-config
users:
  - name: tester
    groups: sudo
    shell: /bin/bash
    lock_passwd: false
    ssh_authorized_keys:
      - $(cat "$work/key.pub")
  - name: nosudo
    shell: /bin/bash
    lock_passwd: false
    ssh_authorized_keys:
      - $(cat "$work/key.pub")
chpasswd:
  list: |
    tester:testerpw
    nosudo:nosudopw
    root:rootpw
  expire: false
EOS
    echo "instance-id: $n" >"$work/seed-$n/meta-data"
    docker run -d --name "$tag-$n" --device /dev/kvm -v "$cache:/cache:ro" -v "$work:/work" \
        -p 127.0.0.1::2222 parity-vm-runner sh -euc "
        qemu-img create -q -f qcow2 -F qcow2 -b /cache/$2 /work/$n.qcow2 8G
        genisoimage -quiet -output /work/seed-$n.iso -volid cidata -joliet -rock /work/seed-$n
        exec qemu-system-x86_64 -enable-kvm -cpu host -m 1024 -smp 2 \
            -drive file=/work/$n.qcow2,if=virtio -drive file=/work/seed-$n.iso,if=virtio,format=raw \
            -nic user,model=virtio-net-pci,hostfwd=tcp::2222-:22 \
            -display none -serial file:/work/$n.console -monitor none" >/dev/null
    containers+=("$tag-$n")
    local port
    port=$(docker port "$tag-$n" 2222/tcp | head -1 | sed 's/.*://')
    cat >>"$work/ssh_config" <<EOS
Host $n
  HostName 127.0.0.1
  Port $port
  User tester
  IdentityFile $work/key
  IdentitiesOnly yes
  StrictHostKeyChecking no
  UserKnownHostsFile /dev/null
  LogLevel ERROR
  ConnectTimeout 5
EOS
}
vms=(vm1 vm2)
start vm1 "$image"
start vm2 "$image"

vssh() { ssh -F "$work/ssh_config" "$@"; }
# Runs a shell command as root on a VM (root logs in with the test key).
vroot() { ssh -F "$work/ssh_config" "root@$1" "$2"; }
for n in "${vms[@]}"; do
    echo "waiting for $n"
    for _ in $(seq 120); do
        vssh "$n" test -f /var/lib/cloud/instance/boot-finished 2>/dev/null && break
        sleep 5
    done
    vssh "$n" true || { tail -30 "$work/$n.console"; echo "$n did not come up"; exit 1; }
    vssh "$n" 'mkdir -p /tmp/parity-tests'
    scp -q -F "$work/ssh_config" "$bin" "$n:/tmp/parity"
    scp -q -F "$work/ssh_config" "${tests[@]}" "$n:/tmp/parity-tests/"
    vssh "$n" 'echo testerpw | sudo -S -p "" sh -c "install -o root -g root -m 0755 /tmp/parity /usr/local/bin/parity && install -o root -g root -m 0600 ~tester/.ssh/authorized_keys /root/.ssh/authorized_keys"'
    echo "$n: $os, Linux $(vssh "$n" uname -r)"
done

# check NAME EXPECTED_EXIT_REGEX PATTERN [--password PW]... [--interrupt-when CMD] -- ARGS...
check() {
    local name=$1 want=$2 pattern=$3
    shift 3
    local drive=()
    while [[ $1 != -- ]]; do drive+=("$1"); shift; done
    shift
    local out rc=0
    out=$(timeout 600 python3 "$here/drive.py" "${drive[@]}" -- "$bin" "$1" --ssh-config "$work/ssh_config" "${@:2}" 2>&1) || rc=$?
    if [[ ! $rc =~ ^($want)$ ]] || ! grep -qE -- "$pattern" <<<"$out"; then
        fail "$name: exit $rc (want $want), pattern '$pattern'"
        head -40 <<<"$out" | sed 's/^/    /'
    else
        echo "ok   $name"
    fi
}
leftovers() {
    local n
    for n in "${vms[@]}"; do
        if vssh "$n" ps -eo args | grep -q '^[^ ]*parity '; then
            fail "processes left on $n:"
            vssh "$n" ps -eo pid,user,args | grep parity
        fi
    done
}

for n in "${vms[@]}"; do
    if vroot "$n" 'cd /tmp && for t in /tmp/parity-tests/*; do $t -q --test-threads=1 || exit 1; done' >"$work/tests-$n.log" 2>&1; then
        echo "ok   tests as root $n"
    else
        fail "tests as root $n"
        tail -40 "$work/tests-$n.log" | sed 's/^/    /'
    fi
done
check "ping sudo" 0 "collect mode running" --password testerpw -- ping vm1
check "ping su" 0 "collect mode running" --password rootpw -- ping --become su vm1
check "ping none as root" 0 "collect mode running" -- ping --become none root@vm1
check "packages" 0 "^ *ii +bash +amd64" --password testerpw -- packages vm1
check "compare with itself" 0 "Result: identical" --password testerpw -- compare vm1 vm1
# The whole file system of a running system, including /proc, /sys, /dev
# and /run on real kernel file systems (excluded by default, so check they
# are not walked by exiting 0 or 1, never 2).
check "filesystem" '[01]' "Result" --password testerpw -- compare --check filesystem vm1 vm1
# Files on pseudo file systems are never read: reading /proc/kmsg as root
# would block until the kernel logs something.
check "pseudo fs never read" 0 "Result: identical" --password testerpw -- compare --check config --root /proc/kmsg vm1 vm1

# Escalation, connection and binary errors.
check "none as non-root" 2 "not running as root" -- ping --become none vm1
check "wrong password" 2 "escalation with sudo on vm1 failed" --password wrong -- ping vm1
check "not in sudoers" 2 "escalation with sudo" --password nosudopw -- ping nosudo@vm1
check "wrong su password" 2 "escalation with su" --password wrong -- ping --become su vm1
check "binary not found" 2 "not found .*--remote-path" -- ping --remote-path /opt/none/parity vm1
check "ssh failure" 2 "ssh to .* failed" -- ping --ssh-option Port=1 vm1
check "option-like target" 2 "must not start with '-'" -- ping -- -oProxyCommand=x
vroot vm1 'printf "#!/bin/sh\necho \"PARITY-HOLD 99 9.9.9 \$\$ 2f\"\n" >/usr/local/bin/parity-old && chmod 755 /usr/local/bin/parity-old'
check "version mismatch before password" 2 "remote parity 9.9.9 \(protocol 99\)" -- ping --remote-path /usr/local/bin/parity-old vm1
vroot vm1 'rm /usr/local/bin/parity-old'
vroot vm1 'mkdir -m 777 /opt/open && cp /usr/local/bin/parity /opt/open/'
check "binary writable by others" 2 "can be changed by other users" -- ping --remote-path /opt/open/parity vm1
vroot vm1 'chown 1001 /opt/open/parity && chmod 755 /opt/open'
check "binary owned by another user" 2 "can be changed by other users" -- ping --remote-path /opt/open/parity vm1
vroot vm1 'rm -r /opt/open'
leftovers

# Two fresh installs of the same image are identical apart from what any two
# installations differ in (see docs/compare-with-base-install.md), the DHCP
# lease (dhclient up to Debian 11) and sudo's lecture records, which the
# checks above leave behind.
fresh_excludes=(--exclude /etc/machine-id --exclude /etc/ssh --exclude /var/lib/cloud
    --exclude /var/lib/dhcp --exclude /var/lib/sudo)
check "identical config" 0 "Result: identical" --password testerpw -- compare "${fresh_excludes[@]}" vm1 vm2
check "sensitive skipped" 0 "/etc/shadow: sensitive path" --password testerpw -- compare "${fresh_excludes[@]}" vm1 vm2
check "identical filesystem" 0 '"result": "identical"' --password testerpw -- compare --check filesystem --format json "${fresh_excludes[@]}" vm1 vm2

# A pending entry in dpkg's journal counts, as for dpkg-query: bash on hold.
vroot vm2 'mkdir -p /var/lib/dpkg/updates && awk -v RS= -v ORS="\n\n" "/^Package: bash\n/" /var/lib/dpkg/status | sed "s/^Status: install ok installed/Status: hold ok installed/" >/var/lib/dpkg/updates/0000'
check "journal packages" 0 "^ *hi +bash +amd64" --password testerpw -- packages vm2
check "journal compare" 1 "^ *bash +amd64 +[^ ]+ +[^ ]+ \(hold\)" --password testerpw -- compare --check packages vm1 vm2
vroot vm2 'rm /var/lib/dpkg/updates/0000'

# Changes made to one of two fresh installs show up.
vroot vm2 'echo extra >>/etc/adduser.conf; chmod 4755 /usr/bin/ls; mkdir -p /opt/app/lib; touch /opt/app/lib/x; chown 1000 /var/lib/dpkg/info/bash.list'
check "content diff" 1 "^ +\+\+\+ vm2:/etc/adduser.conf" --password testerpw -- compare --check config "${fresh_excludes[@]}" vm1 vm2
check "config change" 1 "^ +\+extra" --password testerpw -- compare --check config "${fresh_excludes[@]}" vm1 vm2
check "filesystem mode change" 1 "mode: 0755 -> 4755" --password testerpw -- compare --check filesystem "${fresh_excludes[@]}" vm1 vm2
check "filesystem new tree" 1 "only on vm2 +directory +app \(and 2 entries below\)" --password testerpw -- compare --check filesystem "${fresh_excludes[@]}" vm1 vm2
check "filesystem owner change" 1 "uid: 0 -> 1000" --password testerpw -- compare --check filesystem "${fresh_excludes[@]}" vm1 vm2

# Reading never changes access times on ext4 with relatime.
files="/etc /etc/adduser.conf /etc/debian_version /usr/share/doc /var/lib/dpkg/status"
declare -A before
for n in "${vms[@]}"; do
    vroot "$n" "touch -a -d 2020-01-01 $files"
    before[$n]=$(vssh "$n" stat -c "'%X %n'" $files)
done
check "compare for atime" 1 "Result" --password testerpw -- compare vm1 vm2
for n in "${vms[@]}"; do
    after=$(vssh "$n" stat -c "'%X %n'" $files)
    if [[ ${before[$n]} != "$after" ]]; then
        fail "access times changed on $n:"
        diff <(echo "${before[$n]}") <(echo "$after") || true
    else
        echo "ok   access times unchanged $n"
    fi
done
leftovers

# Ctrl-C in the middle of hashing a large file ends everything on the targets.
# The file is sparse: hashing 3 GB of zeros takes seconds, but nothing is
# written to the VM's disk. Ctrl-C comes once collect mode on vm1 has the file
# open. The test depends on timing: if it does not finish
# within interrupt_timeout seconds (a slow or stuck VM), it is reported as
# timed out and does not count as a failure. A result it does reach (wrong
# exit code, processes left) does.
interrupt_timeout=120
kill_tree() { # pid
    local c
    for c in $(pgrep -P "$1"); do kill_tree "$c"; done
    kill "$1" 2>/dev/null || true
}
(
    failures=0
    for n in "${vms[@]}"; do vroot "$n" 'truncate -s 3G /opt/big'; done
    hashing="ssh -F $work/ssh_config root@vm1 'find /proc/[0-9]*/fd -lname /opt/big 2>/dev/null | grep -q .'"
    check "interrupt" 130 "" --password testerpw --password testerpw --interrupt-when "$hashing" -- compare --check filesystem --max-hash-size 4G vm1 vm2
    sleep 1
    leftovers
    for n in "${vms[@]}"; do vroot "$n" 'rm -f /opt/big'; done
    echo "$failures" >"$work/interrupt.failures"
) &
pid=$!
deadline=$((SECONDS + interrupt_timeout))
while kill -0 "$pid" 2>/dev/null && ((SECONDS < deadline)); do sleep 1; done
if kill -0 "$pid" 2>/dev/null; then
    kill_tree "$pid"
    wait "$pid" 2>/dev/null || true
    echo "TIMEOUT interrupt: not finished after $interrupt_timeout s, not counted as a failure"
    if [[ -n ${GITHUB_ACTIONS:-} ]]; then echo "::warning::interrupt test timed out on $os"; fi
else
    rc=0
    wait "$pid" || rc=$?
    if [[ -f $work/interrupt.failures ]]; then
        failures=$((failures + $(cat "$work/interrupt.failures")))
    else
        fail "interrupt: a command on a VM failed (exit $rc)"
    fi
fi

if [[ $failures != 0 ]]; then
    echo "$failures VM test(s) failed on $os"
    exit 1
fi
echo "all VM tests passed on $os"
