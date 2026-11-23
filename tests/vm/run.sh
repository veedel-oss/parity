#!/usr/bin/env bash
# Tests on virtual machines with their own kernels (containers share the host
# kernel, see tests/e2e):
#
# - Debian 9 (Linux 4.9) and Debian 13 cloud images: the static binary
#   installed in /usr/local/bin, ping, packages and compare over real ssh with
#   sudo and su, access times on ext4, and the workspace tests run as root.
# - The Linux 3.2 kernel of Debian 7, the oldest supported kernel, booted
#   with an initramfs that holds busybox, the static binary and the
#   workspace tests (see initramfs/init).
#
#   tests/vm/run.sh [all|linux-3.2|debian]
#
# Needs docker with /dev/kvm, ssh, ssh-keygen, curl, sha256sum, sha512sum and
# python3. Downloads are cached in ${XDG_CACHE_HOME:-~/.cache}/parity-vm and
# checked against the checksums below.
set -euo pipefail
what=${1:-all}
case $what in all | linux-3.2 | debian) ;; *) echo "usage: $0 [all|linux-3.2|debian]" >&2; exit 2 ;; esac

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
target=x86_64-unknown-linux-musl
cache=${XDG_CACHE_HOME:-$HOME/.cache}/parity-vm
mkdir -p "$cache"

deb9_url=https://cloud.debian.org/images/openstack/archive/9.13.41-20220629/debian-9.13.41-20220629-openstack-amd64.qcow2
deb9_sha512=759f5360f397326664d55d910f09feaa95719999932122646851ebcae3ef14f64bcc65c1decbc5c11025f78cedba7bbcfb51222f661e9a5a4799e1c3129fc5b9
deb13_url=https://cloud.debian.org/images/cloud/trixie/20261116-2664/debian-13-genericcloud-amd64-20261116-2664.qcow2
deb13_sha512=37cb32b320820417eb04b335afcf489bfb4cecce4a957190d09cb2bffbe02b56a1cadd62d78c6ead92aa7839d5ea65e4260f994bbb4b54490df59fdf11ee1d97
k32_url=http://archive.debian.org/debian/pool/main/l/linux/linux-image-3.2.0-4-amd64_3.2.78-1_amd64.deb
k32_sha256=3083a848d7d2dcfe0dfd8f381d9a33cddab15c3407fee5657138b162ca473eea

fetch() { # name url algorithm sum
    local f=$cache/$1
    if [[ ! -f $f ]] || ! echo "$4  $f" | "$3sum" -c --status; then
        echo "downloading $2"
        curl -fL --progress-bar -o "$f.part" "$2"
        echo "$4  $f.part" | "$3sum" -c --status || { echo "checksum mismatch: $2"; exit 1; }
        mv "$f.part" "$f"
    fi
}
fetch deb9.qcow2 "$deb9_url" sha512 "$deb9_sha512"
fetch deb13.qcow2 "$deb13_url" sha512 "$deb13_sha512"
fetch linux-3.2.deb "$k32_url" sha256 "$k32_sha256"

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
runner=(docker run --rm --device /dev/kvm -v "$cache:/cache:ro" -v "$work:/work")

failures=0
fail() { echo "FAIL $*"; failures=$((failures + 1)); }

# --- Linux 3.2 -------------------------------------------------------------

if [[ $what != debian ]]; then
mkdir -p "$work/initramfs/bin" "$work/initramfs/tests"
cp "$here/initramfs/init" "$work/initramfs/"
cp "$here/initramfs/ssh" "$here/initramfs/sudo" "$bin" "$work/initramfs/bin/"
cp "${tests[@]}" "$work/initramfs/tests/"
"${runner[@]}" parity-vm-runner sh -euc '
    cd /work/initramfs
    mkdir -p dev proc sys tmp root etc home/tester
    cp /bin/busybox bin/
    ln -s busybox bin/sh
    chmod 755 init bin/* tests/*
    mknod -m 600 dev/console c 5 1
    printf "root:x:0:0::/root:/bin/sh\ntester:x:1000:1000::/home/tester:/bin/sh\n" >etc/passwd
    printf "root:x:0:\ntester:x:1000:\n" >etc/group
    chown -R 0:0 . && chown 1000:1000 home/tester && chmod 1777 tmp
    find . | cpio -o -H newc --quiet | gzip >/work/initrd.gz
    dpkg-deb -x /cache/linux-3.2.deb /work/k32
    cp /work/k32/boot/vmlinuz-3.2.0-4-amd64 /work/vmlinuz-3.2'
echo "booting Linux 3.2"
containers+=("$tag-k32")
timeout 1800 "${runner[@]}" --name "$tag-k32" parity-vm-runner qemu-system-x86_64 -enable-kvm -cpu host -m 2048 -smp 2 \
    -kernel /work/vmlinuz-3.2 -initrd /work/initrd.gz -append "console=ttyS0 panic=-1 quiet" \
    -display none -serial stdio -monitor none -no-reboot </dev/null | tr -d '\r' >"$work/k32.log" || true
grep -E '^(VM-KERNEL|ok |FAIL|    )' "$work/k32.log" || true
if ! grep -q '^VM-DONE failures=0$' "$work/k32.log"; then
    grep -q '^VM-DONE' "$work/k32.log" || { echo "console output of the Linux 3.2 VM:"; tail -40 "$work/k32.log"; }
    fail "Linux 3.2"
fi
fi

# --- Debian 9 and 13 ---------------------------------------------------------

if [[ $what != linux-3.2 ]]; then
ssh-keygen -q -t ed25519 -N '' -f "$work/key"
start() { # name image
    local n=$1
    mkdir -p "$work/seed-$n"
    cat >"$work/seed-$n/user-data" <<EOF
#cloud-config
users:
  - name: tester
    groups: sudo
    shell: /bin/bash
    lock_passwd: false
    ssh_authorized_keys:
      - $(cat "$work/key.pub")
chpasswd:
  list: |
    tester:testerpw
    root:rootpw
  expire: false
EOF
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
    cat >>"$work/ssh_config" <<EOF
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
EOF
}
vms=(deb9 deb13)
start deb9 deb9.qcow2
start deb13 deb13.qcow2

vssh() { ssh -F "$work/ssh_config" "$@"; }
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
    vssh "$n" 'echo testerpw | sudo -S -p "" install -o root -g root -m 0755 /tmp/parity /usr/local/bin/parity'
    echo "$n: Linux $(vssh "$n" uname -r)"
done

# check NAME EXPECTED_EXIT_REGEX PATTERN [--password PW]... -- ARGS...
check() {
    local name=$1 want=$2 pattern=$3
    shift 3
    local drive=()
    while [[ $1 != -- ]]; do drive+=("$1"); shift; done
    shift
    local out rc=0
    out=$(timeout 600 python3 "$root/tests/e2e/drive.py" "${drive[@]}" -- "$bin" "$1" --ssh-config "$work/ssh_config" "${@:2}" 2>&1) || rc=$?
    if [[ ! $rc =~ ^($want)$ ]] || ! grep -qE -- "$pattern" <<<"$out"; then
        fail "$name: exit $rc (want $want), pattern '$pattern'"
        sed 's/^/    /' <<<"$out" | head -40
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
    if vssh "$n" 'echo testerpw | sudo -S -p "" sh -c "cd /tmp && for t in /tmp/parity-tests/*; do \$t -q --test-threads=1 || exit 1; done"' >"$work/tests-$n.log" 2>&1; then
        echo "ok   tests as root $n"
    else
        fail "tests as root $n"
        tail -40 "$work/tests-$n.log" | sed 's/^/    /'
    fi
    check "ping sudo $n" 0 "collect mode running" --password testerpw -- ping "$n"
    check "ping su $n" 0 "collect mode running" --password rootpw -- ping --become su "$n"
    check "packages $n" 0 "^ +bash +amd64" --password testerpw -- packages "$n"
    check "compare $n with itself" 0 "Result: identical" --password testerpw -- compare "$n" "$n"
    # The whole file system of a running system, including /proc, /sys, /dev
    # and /run on real kernel file systems (excluded by default, so check
    # they are not walked by exiting 0 or 1, never 2).
    check "filesystem $n" '[01]' "Result" --password testerpw -- compare --check filesystem "$n" "$n"
done
leftovers
check "compare deb9 deb13" 1 "^Packages: [0-9]+ differences" --password testerpw -- compare deb9 deb13
check "content diff" 1 "^ +\+\+\+ deb13:/etc/adduser.conf" --password testerpw -- compare --check config deb9 deb13

# Reading never changes access times on ext4 with relatime.
files="/etc /etc/adduser.conf /etc/debian_version /usr/share/doc /var/lib/dpkg/status"
declare -A before
for n in "${vms[@]}"; do
    vssh "$n" "echo testerpw | sudo -S -p '' touch -a -d 2020-01-01 $files"
    before[$n]=$(vssh "$n" stat -c "'%X %n'" $files)
done
check "compare for atime" 1 "Result" --password testerpw -- compare deb9 deb13
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
fi

if [[ $failures != 0 ]]; then
    echo "$failures VM test(s) failed"
    exit 1
fi
echo "all VM tests passed"
