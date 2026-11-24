#!/usr/bin/env bash
#
# End-to-end (e2e) tests on virtual machines.
#
# Usage: tests/e2e/run.sh OS
#        tests/e2e/run.sh --image-url OS
#
#   OS   debian-10, debian-11, debian-12 or debian-13
#
# With --image-url, only prints the URL of the cloud image of OS. CI uses it
# for the key of its image cache.
#
# Tests what only a real system can show: an own kernel, real ssh, sudo and
# su. Boots two VMs, vm1 and vm2, from the latest cloud image of OS, installs
# the static binary in /usr/local/bin on both and tests:
#
#   - the workspace tests, run as root on each VM
#   - ping, packages and compare with sudo, with su and as root
#   - escalation and connection errors, the checks of the installed binary
#   - two fresh installs compare as identical, changes made to one show up
#   - dpkg's journal
#   - access times on ext4
#   - no process left behind after errors and Ctrl-C
#
# Prints "ok   NAME" or "FAIL NAME" (followed by the output) for each test and
# exits with 1 if any test failed.
#
# Needs docker with /dev/kvm, ssh, ssh-keygen, curl and python3. qemu runs in
# a container built from the Dockerfile next to this script. The image is
# cached in ${XDG_CACHE_HOME:-~/.cache}/parity-vm and downloaded again when
# the one on the server is newer. Also run in CI, one job per OS
# (.github/workflows/e2e.yml).

set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)

# ---------------------------------------------------------------------------
# Settings
# ---------------------------------------------------------------------------

# Supported operating systems.
oses=(debian-10 debian-11 debian-12 debian-13)

# Rust target of the static binary.
target=x86_64-unknown-linux-musl

# Downloaded cloud images, kept between runs.
cache=${XDG_CACHE_HOME:-$HOME/.cache}/parity-vm

# The two VMs. The names are also their ssh host names.
vms=(vm1 vm2)

# Users on the VMs, created by cloud-init (see write_seed). All of them log in
# with the test key, root only once install_parity has run.
#
#   tester   password testerpw   in group sudo, the default ssh user
#   nosudo   password nosudopw   not allowed to use sudo
#   root     password rootpw     for su

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# image_url OS
#
# Prints the URL of the latest genericcloud image of OS from cloud.debian.org.
# Fails for an OS that is not supported.
image_url() {
    local distro release codename
    case $1 in
        debian-10) distro=debian; release=10; codename=buster ;;
        debian-11) distro=debian; release=11; codename=bullseye ;;
        debian-12) distro=debian; release=12; codename=bookworm ;;
        debian-13) distro=debian; release=13; codename=trixie ;;
        *) return 1 ;;
    esac
    case $distro in
        debian)
            echo "https://cloud.debian.org/images/cloud/${codename}/latest/debian-${release}-genericcloud-amd64.qcow2"
            return 0
            ;;
    esac
    return 1
}

# download_image URL FILE
#
# Downloads URL to FILE. If FILE exists, downloads only when the server has a
# newer one (curl -z compares with the modification time of FILE, -R sets it
# to the server's). An unfinished download never replaces FILE.
download_image() {
    local url=$1 file=$2
    rm -f "$file.part"
    if [[ -f $file ]]; then
        curl -fsSL -R -z "$file" -o "$file.part" "$url"
    else
        echo "downloading $url"
        curl -fL -R --progress-bar -o "$file.part" "$url"
    fi
    # Missing or empty if FILE is up to date.
    if [[ -s $file.part ]]; then
        mv "$file.part" "$file"
    fi
    rm -f "$file.part"
}

# test_executables
#
# Builds the workspace tests for the static target without running them and
# prints the path of each test executable, one per line.
test_executables() {
    cargo test --manifest-path "$root/Cargo.toml" --workspace --locked \
        --target $target --no-run --message-format=json 2>/dev/null |
        python3 -c '
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]:
        print(m["executable"])'
}

# cleanup
#
# Runs on exit: removes the containers, and with them the VMs, and $work.
# Files the containers wrote to $work are owned by root, so a container
# removes them.
cleanup() {
    local c
    for c in "${containers[@]}"; do
        docker rm -f "$c" >/dev/null 2>&1 || true
    done
    docker run --rm -v "$work:/work" parity-vm-runner sh -c 'rm -rf /work/*' 2>/dev/null || true
    rm -rf "$work"
}

# write_seed VM
#
# Writes the cloud-init configuration of VM to $work/seed-VM: the users listed
# under Settings, each with the test key.
write_seed() {
    local vm=$1 dir=$work/seed-$1 key
    key=$(cat "$work/key.pub")
    mkdir -p "$dir"
    cat >"$dir/user-data" <<EOS
#cloud-config
users:
  - name: tester
    groups: sudo
    shell: /bin/bash
    lock_passwd: false
    ssh_authorized_keys:
      - $key
  - name: nosudo
    shell: /bin/bash
    lock_passwd: false
    ssh_authorized_keys:
      - $key
chpasswd:
  list: |
    tester:testerpw
    nosudo:nosudopw
    root:rootpw
  expire: false
EOS
    echo "instance-id: $vm" >"$dir/meta-data"
}

# start_vm VM IMAGE
#
# Boots VM from IMAGE (a file in $cache) in the container $tag-VM and adds
# "Host VM" to $work/ssh_config. Does not wait until the VM is up.
#
# The VM writes to its own overlay disk in $work, IMAGE stays unchanged.
# cloud-init reads the seed from an ISO with the volume label cidata (its
# NoCloud data source). The serial console goes to $work/VM.console. ssh
# reaches the VM's port 22 through port 2222 of the container, which docker
# publishes on a free port of 127.0.0.1.
start_vm() {
    local vm=$1 image=$2 container=$tag-$1 port
    write_seed "$vm"
    docker run -d --name "$container" --device /dev/kvm \
        -v "$cache:/cache:ro" -v "$work:/work" -p 127.0.0.1::2222 \
        parity-vm-runner sh -euc "
        qemu-img create -q -f qcow2 -F qcow2 -b /cache/$image /work/$vm.qcow2 8G
        genisoimage -quiet -output /work/seed-$vm.iso -volid cidata -joliet -rock /work/seed-$vm
        exec qemu-system-x86_64 -enable-kvm -cpu host -m 1024 -smp 2 \
            -drive file=/work/$vm.qcow2,if=virtio \
            -drive file=/work/seed-$vm.iso,if=virtio,format=raw \
            -nic user,model=virtio-net-pci,hostfwd=tcp::2222-:22 \
            -display none -serial file:/work/$vm.console -monitor none" >/dev/null
    containers+=("$container")
    port=$(docker port "$container" 2222/tcp | head -1 | sed 's/.*://')
    cat >>"$work/ssh_config" <<EOS
Host $vm
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

# vssh [USER@]VM COMMAND...
#
# Runs COMMAND on VM over ssh, as tester unless USER is given.
vssh() { ssh -F "$work/ssh_config" "$@"; }

# vroot VM COMMAND
#
# Runs the shell command COMMAND as root on VM.
vroot() { ssh -F "$work/ssh_config" "root@$1" "$2"; }

# wait_for_vm VM
#
# Waits until cloud-init on VM has finished, trying 120 times 5 seconds apart.
# If VM cannot be reached then, shows the end of its console and exits.
wait_for_vm() {
    local vm=$1
    echo "waiting for $vm"
    for _ in $(seq 120); do
        vssh "$vm" test -f /var/lib/cloud/instance/boot-finished 2>/dev/null && break
        sleep 5
    done
    if ! vssh "$vm" true; then
        tail -30 "$work/$vm.console"
        echo "$vm did not come up"
        exit 1
    fi
}

# install_parity VM
#
# Copies the binary and the test executables to VM, installs the binary as
# /usr/local/bin/parity, owned by root (the CLI refuses a binary other users
# can change), and lets root log in with the test key.
install_parity() {
    local vm=$1
    vssh "$vm" 'mkdir -p /tmp/parity-tests'
    scp -q -F "$work/ssh_config" "$bin" "$vm:/tmp/parity"
    scp -q -F "$work/ssh_config" "${tests[@]}" "$vm:/tmp/parity-tests/"
    vssh "$vm" 'echo testerpw | sudo -S -p "" sh -c "
        install -o root -g root -m 0755 /tmp/parity /usr/local/bin/parity &&
        install -o root -g root -m 0600 ~tester/.ssh/authorized_keys /root/.ssh/authorized_keys"'
}

# ok NAME
# fail MESSAGE
#
# Report a test that passed or failed. fail counts the failures.
ok() { echo "ok   $*"; }
fail() { echo "FAIL $*"; failures=$((failures + 1)); }

# indent
#
# Indents its input, to set the output of a failed test apart.
indent() { sed 's/^/    /'; }

# check NAME EXIT PATTERN [DRIVE-OPTION]... -- SUBCOMMAND [ARG]...
#
# Runs "parity SUBCOMMAND ARG..." on this machine against the VMs. Passes if
# its exit code matches the regular expression EXIT and its output (stdout and
# stderr) has a line that matches the extended regular expression PATTERN.
#
# The command runs on a pseudo terminal under drive.py, which types passwords
# at the prompts as a person would. DRIVE-OPTIONs go to drive.py:
#
#   --password PW         typed at the next password prompt; the last one is
#                         repeated, without any an empty line is typed
#   --interrupt-when CMD  types Ctrl-C as soon as the shell command CMD
#                         succeeds
#
# Gives up after 10 minutes.
check() {
    local name=$1 want=$2 pattern=$3
    shift 3
    local drive=()
    while [[ $1 != -- ]]; do
        drive+=("$1")
        shift
    done
    shift
    local subcommand=$1
    shift
    local out rc=0
    out=$(timeout 600 python3 "$here/drive.py" "${drive[@]}" -- \
        "$bin" "$subcommand" --ssh-config "$work/ssh_config" "$@" 2>&1) || rc=$?
    if [[ $rc =~ ^($want)$ ]] && grep -qE -- "$pattern" <<<"$out"; then
        ok "$name"
    else
        fail "$name: exit $rc (want $want), pattern '$pattern'"
        head -40 <<<"$out" | indent
    fi
}

# check_no_processes_left
#
# Fails if a parity process (collect mode or the holder of the session) is
# still running on a VM.
check_no_processes_left() {
    local vm
    for vm in "${vms[@]}"; do
        if vssh "$vm" ps -eo args | grep -q '^[^ ]*parity '; then
            fail "processes left on $vm:"
            vssh "$vm" ps -eo pid,user,args | grep parity
        fi
    done
}

# kill_tree PID
#
# Kills PID and all processes below it.
kill_tree() {
    local c
    for c in $(pgrep -P "$1"); do
        kill_tree "$c"
    done
    kill "$1" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# Preparation
# ---------------------------------------------------------------------------

# usage
#
# Prints how to call this script and exits with 2.
usage() {
    echo "usage: $0 [--image-url] OS (one of: ${oses[*]})" >&2
    exit 2
}

if [[ $# == 2 && $1 == --image-url ]]; then
    image_url "$2" || usage
    exit 0
fi
os=${1:-}
if [[ $# != 1 ]] || ! url=$(image_url "$os"); then
    usage
fi

mkdir -p "$cache"
image=$os.qcow2
download_image "$url" "$cache/$image"

cargo build --manifest-path "$root/Cargo.toml" --release --locked --target $target
bin=$root/target/$target/release/parity
mapfile -t tests < <(test_executables)

# Files of this run: the test key, ssh_config, the VMs' disks, seeds and
# consoles, test logs.
work=$(mktemp -d)
# Prefix of this run's container names.
tag=parity-vm-$$
containers=()
trap cleanup EXIT

docker build -q -t parity-vm-runner "$here" >/dev/null
ssh-keygen -q -t ed25519 -N '' -f "$work/key"

for vm in "${vms[@]}"; do
    start_vm "$vm" "$image"
done
for vm in "${vms[@]}"; do
    wait_for_vm "$vm"
    install_parity "$vm"
    echo "$vm: $os, Linux $(vssh "$vm" uname -r)"
done

failures=0

# ---------------------------------------------------------------------------
# Workspace tests as root
# ---------------------------------------------------------------------------

# Each test executable runs on its own, one test at a time. The first one
# that fails stops the run on that VM.
# shellcheck disable=SC2016 # $t expands on the VM
run_tests='
    cd /tmp || exit 1
    for t in /tmp/parity-tests/*; do
        "$t" -q --test-threads=1 || exit 1
    done'
for vm in "${vms[@]}"; do
    if vroot "$vm" "$run_tests" >"$work/tests-$vm.log" 2>&1; then
        ok "tests as root $vm"
    else
        fail "tests as root $vm"
        tail -40 "$work/tests-$vm.log" | indent
    fi
done

# ---------------------------------------------------------------------------
# Commands with sudo, with su and as root
# ---------------------------------------------------------------------------

check "ping sudo" 0 "collect mode running" \
    --password testerpw -- ping vm1
check "ping su" 0 "collect mode running" \
    --password rootpw -- ping --become su vm1
check "ping none as root" 0 "collect mode running" \
    -- ping --become none root@vm1
check "packages" 0 "^ *ii +bash +amd64" \
    --password testerpw -- packages vm1
check "compare with itself" 0 "Result: identical" \
    --password testerpw -- compare vm1 vm1

# The whole file system of a running system, including /proc, /sys, /dev and
# /run on real kernel file systems. These are excluded by default, so check
# that they are not walked: exit 0 or 1, never 2.
check "filesystem" '[01]' "Result" \
    --password testerpw -- compare --check filesystem vm1 vm1

# Files on pseudo file systems are never read: reading /proc/kmsg as root
# would block until the kernel logs something.
check "pseudo fs never read" 0 "Result: identical" \
    --password testerpw -- compare --check etc --etc-dirs /proc/kmsg vm1 vm1

# ---------------------------------------------------------------------------
# Escalation, connection and binary errors
# ---------------------------------------------------------------------------

check "none as non-root" 2 "not running as root" \
    -- ping --become none vm1
check "wrong password" 2 "escalation with sudo on vm1 failed" \
    --password wrong -- ping vm1
check "not in sudoers" 2 "escalation with sudo" \
    --password nosudopw -- ping nosudo@vm1
check "wrong su password" 2 "escalation with su" \
    --password wrong -- ping --become su vm1
check "binary not found" 2 "not found .*--parity-path" \
    -- ping --parity-path /opt/none/parity vm1
check "ssh failure" 2 "ssh to .* failed" \
    -- ping --ssh-option Port=1 vm1
check "option-like target" 2 "must not start with '-'" \
    -- ping -- -oProxyCommand=x

# A parity with another protocol on the target is refused before anyone is
# asked for a password. The stand-in prints the hold line of protocol 99 and
# version 9.9.9 with its PID and the path "/" in hex (see "parity hold" in
# docs/architecture.md).
vroot vm1 'cat >/usr/local/bin/parity-old && chmod 755 /usr/local/bin/parity-old' <<'EOS'
#!/bin/sh
echo "PARITY-HOLD 99 9.9.9 $$ 2f"
EOS
check "version mismatch before password" 2 "remote parity 9.9.9 \(protocol 99\)" \
    -- ping --parity-path /usr/local/bin/parity-old vm1
vroot vm1 'rm /usr/local/bin/parity-old'

# A binary that users other than root can change is refused: first one in a
# directory everyone can write to, then one owned by another user.
vroot vm1 'mkdir -m 777 /opt/open && cp /usr/local/bin/parity /opt/open/'
check "binary writable by others" 2 "can be changed by other users" \
    -- ping --parity-path /opt/open/parity vm1
vroot vm1 'chown 1001 /opt/open/parity && chmod 755 /opt/open'
check "binary owned by another user" 2 "can be changed by other users" \
    -- ping --parity-path /opt/open/parity vm1
vroot vm1 'rm -r /opt/open'

check_no_processes_left

# ---------------------------------------------------------------------------
# Two fresh installs
# ---------------------------------------------------------------------------

# Two fresh installs of the same image are identical apart from what any two
# installations differ in (see docs/compare-with-base-install.md), the DHCP
# lease (dhclient up to Debian 11) and sudo's lecture records, which the
# checks above leave behind.
fresh_excludes=(
    --exclude /etc/machine-id
    --exclude /etc/ssh
    --exclude /var/lib/cloud
    --exclude /var/lib/dhcp
    --exclude /var/lib/sudo
)
check "identical config" 0 "Result: identical" \
    --password testerpw -- compare "${fresh_excludes[@]}" vm1 vm2
check "sensitive skipped" 0 "/etc/shadow: sensitive path" \
    --password testerpw -- compare "${fresh_excludes[@]}" vm1 vm2
check "identical filesystem" 0 '"result": "identical"' \
    --password testerpw -- compare --check filesystem --format json "${fresh_excludes[@]}" vm1 vm2

# A pending entry in dpkg's journal counts, as for dpkg-query: bash on hold.
# The entry is bash's record from the status database (awk in paragraph mode
# prints it) with the status changed to hold.
vroot vm2 '
    mkdir -p /var/lib/dpkg/updates &&
    awk -v RS= -v ORS="\n\n" "/^Package: bash\n/" /var/lib/dpkg/status |
        sed "s/^Status: install ok installed/Status: hold ok installed/" \
        >/var/lib/dpkg/updates/0000'
check "journal packages" 0 "^ *hi +bash +amd64" \
    --password testerpw -- packages vm2
check "journal compare" 1 "^ *bash +amd64 +[^ ]+ +[^ ]+ \(hold\)" \
    --password testerpw -- compare --check packages vm1 vm2
vroot vm2 'rm /var/lib/dpkg/updates/0000'

# Changes made to one of two fresh installs show up. On vm2: a line added to
# a configuration file, the setuid bit set on a binary, a new directory tree
# and another owner for a file.
vroot vm2 '
    echo extra >>/etc/adduser.conf
    chmod 4755 /usr/bin/ls
    mkdir -p /opt/app/lib
    touch /opt/app/lib/x
    chown 1000 /var/lib/dpkg/info/bash.list'
check "content diff" 1 "^ +\+\+\+ vm2:/etc/adduser.conf" \
    --password testerpw -- compare --check etc "${fresh_excludes[@]}" vm1 vm2
check "config change" 1 "^ +\+extra" \
    --password testerpw -- compare --check etc "${fresh_excludes[@]}" vm1 vm2
check "filesystem mode change" 1 "mode: 0755 -> 4755" \
    --password testerpw -- compare --check filesystem "${fresh_excludes[@]}" vm1 vm2
check "filesystem new tree" 1 "only on vm2 +directory +app \(and 2 entries below\)" \
    --password testerpw -- compare --check filesystem "${fresh_excludes[@]}" vm1 vm2
check "filesystem owner change" 1 "uid: 0 -> 1000" \
    --password testerpw -- compare --check filesystem "${fresh_excludes[@]}" vm1 vm2

# ---------------------------------------------------------------------------
# Access times
# ---------------------------------------------------------------------------

# Reading never changes access times on ext4 with relatime. relatime updates
# an access time that is more than a day old, so the access times are set
# back to 2020 first: then any read would show.
atime_files=(/etc /etc/adduser.conf /etc/debian_version /usr/share/doc /var/lib/dpkg/status)
declare -A atimes_before
for vm in "${vms[@]}"; do
    vroot "$vm" "touch -a -d 2020-01-01 ${atime_files[*]}"
    atimes_before[$vm]=$(vssh "$vm" stat -c "'%X %n'" "${atime_files[@]}")
done
check "compare for atime" 1 "Result" \
    --password testerpw -- compare vm1 vm2
for vm in "${vms[@]}"; do
    atimes_after=$(vssh "$vm" stat -c "'%X %n'" "${atime_files[@]}")
    if [[ ${atimes_before[$vm]} == "$atimes_after" ]]; then
        ok "access times unchanged $vm"
    else
        fail "access times changed on $vm:"
        diff <(echo "${atimes_before[$vm]}") <(echo "$atimes_after") || true
    fi
done

check_no_processes_left

# ---------------------------------------------------------------------------
# Ctrl-C
# ---------------------------------------------------------------------------

# Ctrl-C in the middle of hashing a large file ends everything on the targets.
# The file is sparse: hashing 16 GB of zeros takes seconds even with the
# CPU's SHA instructions, but nothing is written to the VM's disk. Ctrl-C
# comes once collect mode on vm1 has the file open. The test depends on
# timing: if it does not finish within interrupt_timeout seconds (a slow or
# stuck VM), it is reported as timed out and does not count as a failure. A
# result it does reach (wrong exit code, processes left) does.
interrupt_timeout=120

# The test runs in a background subshell, so that it can be stopped after
# interrupt_timeout. The subshell counts its own failures and hands them back
# in a file. Without that file, a command on a VM failed and stopped it.
(
    # shellcheck disable=SC2030 # counted here, added up below
    failures=0
    for vm in "${vms[@]}"; do
        vroot "$vm" 'truncate -s 16G /opt/big'
    done
    # Succeeds once a process on vm1 has /opt/big open.
    hashing="ssh -F $work/ssh_config root@vm1 'find /proc/[0-9]*/fd -lname /opt/big 2>/dev/null | grep -q .'"
    # One password for each VM. Exit 130 is 128 + SIGINT. Files up to 32 GB
    # are hashed, so /opt/big is.
    check "interrupt" 130 "" \
        --password testerpw --password testerpw --interrupt-when "$hashing" \
        -- compare --check filesystem --max-hash-size 32G vm1 vm2
    # Gives the processes on the targets a moment to exit.
    sleep 1
    check_no_processes_left
    for vm in "${vms[@]}"; do
        vroot "$vm" 'rm -f /opt/big'
    done
    echo "$failures" >"$work/interrupt.failures"
) &
interrupt_pid=$!

deadline=$((SECONDS + interrupt_timeout))
while kill -0 "$interrupt_pid" 2>/dev/null && ((SECONDS < deadline)); do
    sleep 1
done
if kill -0 "$interrupt_pid" 2>/dev/null; then
    kill_tree "$interrupt_pid"
    wait "$interrupt_pid" 2>/dev/null || true
    echo "TIMEOUT interrupt: not finished after $interrupt_timeout s, not counted as a failure"
    # Shown as a warning on the summary of a GitHub Actions run.
    if [[ -n ${GITHUB_ACTIONS:-} ]]; then
        echo "::warning::interrupt test timed out on $os"
    fi
else
    rc=0
    wait "$interrupt_pid" || rc=$?
    if [[ -f $work/interrupt.failures ]]; then
        # shellcheck disable=SC2031 # the subshell's count comes from the file
        failures=$((failures + $(cat "$work/interrupt.failures")))
    else
        fail "interrupt: a command on a VM failed (exit $rc)"
    fi
fi

# ---------------------------------------------------------------------------
# Result
# ---------------------------------------------------------------------------

if [[ $failures != 0 ]]; then
    echo "$failures e2e test(s) failed on $os"
    exit 1
fi
echo "all e2e tests passed on $os"
