#!/usr/bin/env bash
#
# Usage: tests/e2e/run.sh [--image-url] OS
#
# The e2e tests on two VMs (README.md next to this script). --image-url only
# prints the URL of the cloud image, for the cache key in CI.

set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "${here}/../.." && pwd)

# ---------------------------------------------------------------------------
# Settings
# ---------------------------------------------------------------------------

oses=(debian-10 debian-11 debian-12 debian-13)
target=x86_64-unknown-linux-musl
cache=${XDG_CACHE_HOME:-${HOME}/.cache}/parity-vm
# Also their ssh host names.
vms=(vm1 vm2)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

image_url() {
    local distro release codename
    case $1 in
        debian-10) distro=debian; release=10; codename=buster ;;
        debian-11) distro=debian; release=11; codename=bullseye ;;
        debian-12) distro=debian; release=12; codename=bookworm ;;
        debian-13) distro=debian; release=13; codename=trixie ;;
        *) usage ;;
    esac
    case ${distro} in
        debian)
            echo "https://cloud.debian.org/images/cloud/${codename}/latest/debian-${release}-genericcloud-amd64.qcow2"
            ;;
        *) echo "no image for ${distro}" >&2; exit 1 ;;
    esac
}

# download_image URL FILE
#
# Only if the server's file is newer: -z compares with the time of FILE, -R
# sets it. An unfinished download never replaces FILE.
download_image() {
    local url=$1 file=$2
    rm -f "${file}.part"
    if [[ -f ${file} ]]; then
        curl -fsSL -R -z "${file}" -o "${file}.part" "${url}"
    else
        echo "downloading ${url}"
        curl -fL -R --progress-bar -o "${file}.part" "${url}"
    fi
    # Missing or empty if FILE is up to date.
    if [[ -s ${file}.part ]]; then
        mv "${file}.part" "${file}"
    fi
    rm -f "${file}.part"
}

test_executables() {
    (cd "${root}" && cargo test --workspace --locked \
        --target "${target}" --no-run --message-format=json 2>/dev/null) |
        python3 -c '
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]:
        print(m["executable"])'
}

# Files the containers wrote to $work are owned by root, so a container
# removes them.
cleanup() {
    local c
    for c in "${containers[@]}"; do
        docker rm -f "${c}" >/dev/null 2>&1 || true
    done
    docker run --rm -v "${work}:/work" parity-vm-runner sh -c 'rm -rf /work/*' 2>/dev/null || true
    rm -rf "${work}"
}

# tester can use sudo, nosudo cannot, root's password is for su.
write_seed() {
    local vm=$1 dir=${work}/seed-$1 key
    key=$(cat "${work}/key.pub")
    mkdir -p "${dir}"
    cat >"${dir}/user-data" <<EOS
#cloud-config
users:
  - name: tester
    groups: sudo
    shell: /bin/bash
    lock_passwd: false
    ssh_authorized_keys:
      - ${key}
  - name: nosudo
    shell: /bin/bash
    lock_passwd: false
    ssh_authorized_keys:
      - ${key}
chpasswd:
  list: |
    tester:testerpw
    nosudo:nosudopw
    root:rootpw
  expire: false
EOS
    echo "instance-id: ${vm}" >"${dir}/meta-data"
}

# start_vm VM IMAGE
#
# Does not wait until the VM is up. cloud-init's NoCloud data source reads the
# seed from an ISO labelled cidata.
start_vm() {
    local vm=$1 image=$2 container=${tag}-$1 port
    write_seed "${vm}"
    docker run -d --name "${container}" --device /dev/kvm \
        -v "${cache}:/cache:ro" -v "${work}:/work" -p 127.0.0.1::2222 \
        parity-vm-runner sh -euc "
        qemu-img create -q -f qcow2 -F qcow2 -b /cache/${image} /work/${vm}.qcow2 8G
        genisoimage -quiet -output /work/seed-${vm}.iso -volid cidata -joliet -rock /work/seed-${vm}
        exec qemu-system-x86_64 -enable-kvm -cpu host -m 1024 -smp 2 \
            -drive file=/work/${vm}.qcow2,if=virtio \
            -drive file=/work/seed-${vm}.iso,if=virtio,format=raw \
            -nic user,model=virtio-net-pci,hostfwd=tcp::2222-:22 \
            -display none -serial file:/work/${vm}.console -monitor none" >/dev/null
    containers+=("${container}")
    port=$(docker port "${container}" 2222/tcp | head -1 | sed 's/.*://')
    cat >>"${work}/ssh_config" <<EOS
Host ${vm}
  HostName 127.0.0.1
  Port ${port}
  User tester
  IdentityFile ${work}/key
  IdentitiesOnly yes
  StrictHostKeyChecking no
  UserKnownHostsFile /dev/null
  LogLevel ERROR
  ConnectTimeout 5
EOS
}

vssh() { ssh -F "${work}/ssh_config" "$@"; }
vroot() { ssh -F "${work}/ssh_config" "root@$1" "$2"; }

wait_for_vm() {
    local vm=$1
    echo "waiting for ${vm}"
    for _ in $(seq 120); do
        # shellcheck disable=SC2310 # vssh is one ssh command, set -e changes nothing in it
        vssh "${vm}" test -f /var/lib/cloud/instance/boot-finished 2>/dev/null && break
        sleep 5
    done
    # shellcheck disable=SC2310 # vssh is one ssh command, set -e changes nothing in it
    if ! vssh "${vm}" true; then
        tail -30 "${work}/${vm}.console"
        echo "${vm} did not come up"
        exit 1
    fi
}

# Owned by root: the CLI refuses a binary that other users can change.
install_parity() {
    local vm=$1
    vssh "${vm}" 'mkdir -p /tmp/parity-tests'
    scp -q -F "${work}/ssh_config" "${bin}" "${vm}:/tmp/parity"
    scp -q -F "${work}/ssh_config" "${tests[@]}" "${vm}:/tmp/parity-tests/"
    vssh "${vm}" 'echo testerpw | sudo -S -p "" sh -c "
        install -o root -g root -m 0755 /tmp/parity /usr/local/bin/parity &&
        install -o root -g root -m 0600 ~tester/.ssh/authorized_keys /root/.ssh/authorized_keys"'
}

ok() { echo "ok   $*"; }
fail() { echo "FAIL $*"; failures=$((failures + 1)); }
indent() { sed 's/^/    /'; }

# check NAME EXIT PATTERN [DRIVE-OPTION]... -- SUBCOMMAND [ARG]...
#
# Passes if the exit code matches the regular expression EXIT and a line of
# the output matches the extended regular expression PATTERN. The options of
# drive.py are described there.
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
    out=$(timeout 600 python3 "${here}/drive.py" "${drive[@]}" -- \
        "${bin}" "${subcommand}" --ssh-config "${work}/ssh_config" "$@" 2>&1) || rc=$?
    if [[ ${rc} =~ ^(${want})$ ]] && grep -qE -- "${pattern}" <<<"${out}"; then
        ok "${name}"
    else
        fail "${name}: exit ${rc} (want ${want}), pattern '${pattern}'"
        head -40 <<<"${out}" | indent
    fi
}

check_no_processes_left() {
    local vm
    for vm in "${vms[@]}"; do
        # shellcheck disable=SC2310 # vssh is one ssh command, set -e changes nothing in it
        if vssh "${vm}" ps -eo args | grep -q '^[^ ]*parity '; then
            fail "processes left on ${vm}:"
            vssh "${vm}" ps -eo pid,user,args | grep parity
        fi
    done
}

kill_tree() {
    local c
    for c in $(pgrep -P "$1"); do
        kill_tree "${c}"
    done
    kill "$1" 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# Preparation
# ---------------------------------------------------------------------------

usage() {
    echo "usage: $0 [--image-url] OS (one of: ${oses[*]})" >&2
    exit 2
}

if [[ $# == 2 && $1 == --image-url ]]; then
    image_url "$2"
    exit 0
fi
[[ $# == 1 ]] || usage
os=$1
# Exits with the usage for an unknown OS (set -e).
url=$(image_url "${os}")

mkdir -p "${cache}"
image=${os}.qcow2
download_image "${url}" "${cache}/${image}"

(cd "${root}" && cargo build --release --locked --target "${target}")
bin=${root}/target/${target}/release/parity
test_list=$(test_executables)
mapfile -t tests <<<"${test_list}"

work=$(mktemp -d)
tag=parity-vm-$$
containers=()
trap cleanup EXIT

docker build -q -t parity-vm-runner "${here}" >/dev/null
ssh-keygen -q -t ed25519 -N '' -f "${work}/key"

for vm in "${vms[@]}"; do
    start_vm "${vm}" "${image}"
done
for vm in "${vms[@]}"; do
    wait_for_vm "${vm}"
    install_parity "${vm}"
    kernel=$(vssh "${vm}" uname -r)
    echo "${vm}: ${os}, Linux ${kernel}"
done

failures=0

# ---------------------------------------------------------------------------
# Workspace tests as root
# ---------------------------------------------------------------------------

# shellcheck disable=SC2016 # $t expands on the VM
run_tests='
    cd /tmp || exit 1
    for t in /tmp/parity-tests/*; do
        "$t" -q --test-threads=1 || exit 1
    done'
for vm in "${vms[@]}"; do
    # shellcheck disable=SC2310 # vroot is one ssh command, set -e changes nothing in it
    if vroot "${vm}" "${run_tests}" >"${work}/tests-${vm}.log" 2>&1; then
        ok "tests as root ${vm}"
    else
        fail "tests as root ${vm}"
        tail -40 "${work}/tests-${vm}.log" | indent
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

# /proc, /sys, /dev and /run are excluded by default, so never exit 2.
check "filesystem" '[01]' "Result" \
    --password testerpw -- compare --check filesystem vm1 vm1

# Reading /proc/kmsg would block until the kernel logs something.
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

# Refused before the password prompt. The stand-in prints a hold line of
# protocol 99 (docs/architecture.md, parity hold).
vroot vm1 'cat >/usr/local/bin/parity-old && chmod 755 /usr/local/bin/parity-old' <<'EOS'
#!/bin/sh
echo "PARITY-HOLD 99 9.9.9 $$ 2f"
EOS
check "version mismatch before password" 2 "remote parity 9.9.9 \(protocol 99\)" \
    -- ping --parity-path /usr/local/bin/parity-old vm1
vroot vm1 'rm /usr/local/bin/parity-old'

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

# What any two installs differ in (docs/compare-with-base-install.md), the
# DHCP lease up to Debian 11 and the sudo lecture records of the checks above.
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

# A pending journal entry counts, as for dpkg-query: bash's record, on hold.
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

# relatime updates access times older than a day, so set them back first:
# then any read would show.
atime_files=(/etc /etc/adduser.conf /etc/debian_version /usr/share/doc /var/lib/dpkg/status)
declare -A atimes_before
for vm in "${vms[@]}"; do
    vroot "${vm}" "touch -a -d 2020-01-01 ${atime_files[*]}"
    atimes_before[${vm}]=$(vssh "${vm}" stat -c "'%X %n'" "${atime_files[@]}")
done
check "compare for atime" 1 "Result" \
    --password testerpw -- compare vm1 vm2
for vm in "${vms[@]}"; do
    atimes_after=$(vssh "${vm}" stat -c "'%X %n'" "${atime_files[@]}")
    if [[ ${atimes_before[${vm}]} == "${atimes_after}" ]]; then
        ok "access times unchanged ${vm}"
    else
        fail "access times changed on ${vm}:"
        diff <(echo "${atimes_before[${vm}]}") <(echo "${atimes_after}") || true
    fi
done

check_no_processes_left

# ---------------------------------------------------------------------------
# Ctrl-C
# ---------------------------------------------------------------------------

# Ctrl-C while collect mode hashes a sparse file. It depends on timing, so not
# finishing within interrupt_timeout is reported, but is no failure. The
# subshell hands its failures back in a file; without it, a command failed.
interrupt_timeout=120
(
    # shellcheck disable=SC2030 # counted here, added up below
    failures=0
    for vm in "${vms[@]}"; do
        vroot "${vm}" 'truncate -s 16G /opt/big'
    done
    hashing="ssh -F ${work}/ssh_config root@vm1 'find /proc/[0-9]*/fd -lname /opt/big 2>/dev/null | grep -q .'"
    # 130: 128 + SIGINT.
    check "interrupt" 130 "" \
        --password testerpw --password testerpw --interrupt-when "${hashing}" \
        -- compare --check filesystem --max-hash-size 32G vm1 vm2
    # The processes on the targets need a moment to exit.
    sleep 1
    check_no_processes_left
    for vm in "${vms[@]}"; do
        vroot "${vm}" 'rm -f /opt/big'
    done
    echo "${failures}" >"${work}/interrupt.failures"
) &
interrupt_pid=$!

deadline=$((SECONDS + interrupt_timeout))
while kill -0 "${interrupt_pid}" 2>/dev/null && ((SECONDS < deadline)); do
    sleep 1
done
if kill -0 "${interrupt_pid}" 2>/dev/null; then
    kill_tree "${interrupt_pid}"
    wait "${interrupt_pid}" 2>/dev/null || true
    echo "TIMEOUT interrupt: not finished after ${interrupt_timeout} s, not counted as a failure"
    if [[ -n ${GITHUB_ACTIONS:-} ]]; then
        echo "::warning::interrupt test timed out on ${os}"
    fi
else
    rc=0
    wait "${interrupt_pid}" || rc=$?
    if [[ -f ${work}/interrupt.failures ]]; then
        # shellcheck disable=SC2031 # the subshell's count comes from the file
        failures=$((failures + $(cat "${work}/interrupt.failures")))
    else
        fail "interrupt: a command on a VM failed (exit ${rc})"
    fi
fi

# ---------------------------------------------------------------------------
# Result
# ---------------------------------------------------------------------------

if [[ ${failures} != 0 ]]; then
    echo "${failures} e2e test(s) failed on ${os}"
    exit 1
fi
echo "all e2e tests passed on ${os}"
