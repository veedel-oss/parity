#!/usr/bin/env bash
# End-to-end tests against Debian containers over real ssh, sudo and su.
#
#   tests/e2e/run.sh [path/to/parity]
#
# Needs docker, ssh, ssh-keygen and python3. Builds the static binary if no
# path is given. Containers share the host kernel: this covers the user space
# of Debian 9 and 13 (ssh, sudo, su, dpkg), not their kernels.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
bin=${1:-}
if [[ -z $bin ]]; then
    cargo build --manifest-path "$root/Cargo.toml" --release --locked --target x86_64-unknown-linux-musl
    bin=$root/target/x86_64-unknown-linux-musl/release/parity
fi
bin=$(realpath "$bin")

work=$(mktemp -d)
tag=parity-e2e-$$
containers=()
cleanup() {
    for c in "${containers[@]}"; do docker rm -f "$c" >/dev/null 2>&1 || true; done
    rm -rf "$work"
}
trap cleanup EXIT

ssh-keygen -q -t ed25519 -N '' -f "$work/key"
cp "$work/key.pub" "$work/authorized_keys"
cp "$here/Dockerfile" "$work/"
for release in stretch trixie; do
    docker build -q --build-arg "BASE=debian:$release" -t "parity-e2e:$release" "$work" >/dev/null
done

start() { # name image
    docker run -d --name "$tag-$1" -p 127.0.0.1::22 "$2" >/dev/null
    containers+=("$tag-$1")
    docker cp -q "$bin" "$tag-$1:/usr/local/bin/parity"
    docker exec "$tag-$1" chown root:root /usr/local/bin/parity
    local port
    port=$(docker port "$tag-$1" 22/tcp | head -1 | sed 's/.*://')
    cat >>"$work/ssh_config" <<EOF
Host $1
  HostName 127.0.0.1
  Port $port
  User tester
  IdentityFile $work/key
  IdentitiesOnly yes
  StrictHostKeyChecking no
  UserKnownHostsFile /dev/null
  LogLevel ERROR
EOF
}
start deb9 parity-e2e:stretch
start deb13 parity-e2e:trixie
start deb13b parity-e2e:trixie
sleep 2

failures=0
# check NAME EXPECTED_EXIT PATTERN [--password PW]... -- ARGS...
check() {
    local name=$1 want=$2 pattern=$3
    shift 3
    local drive=()
    while [[ $1 != -- ]]; do drive+=("$1"); shift; done
    shift
    local out rc=0
    out=$(timeout 300 python3 "$here/drive.py" "${drive[@]}" -- "$bin" "$1" --ssh-config "$work/ssh_config" "${@:2}" 2>&1) || rc=$?
    if [[ $rc != "$want" ]] || ! grep -qE -- "$pattern" <<<"$out"; then
        echo "FAIL $name: exit $rc (want $want), pattern '$pattern'"
        sed 's/^/    /' <<<"$out" | head -40
        failures=$((failures + 1))
    else
        echo "ok   $name"
    fi
}

leftovers() {
    local c
    for c in "${containers[@]}"; do
        if docker exec "$c" ps -eo args | grep -q '^[^ ]*parity '; then
            echo "FAIL processes left on $c:"
            docker exec "$c" ps -eo pid,user,args | grep parity
            failures=$((failures + 1))
        fi
    done
}

docker_excludes=(--exclude /.dockerenv --exclude /etc/hostname --exclude /etc/hosts
    --exclude /etc/resolv.conf --exclude /etc/ssh)

for t in deb9 deb13; do
    check "ping sudo $t" 0 "collect mode running" --password testerpw -- ping "$t"
    check "ping su $t" 0 "collect mode running" --password rootpw -- ping --become su "$t"
    check "ping none as root $t" 0 "collect mode running" -- ping --become none "root@$t"
done
check "none as non-root" 2 "not running as root" -- ping --become none deb13
check "wrong password" 2 "escalation with sudo on deb13 failed" --password wrong -- ping deb13
check "not in sudoers" 2 "escalation with sudo" --password nosudopw -- ping nosudo@deb9
check "wrong su password" 2 "escalation with su" --password wrong -- ping --become su deb9
check "binary not found" 2 "not found .*--remote-path" -- ping --remote-path /opt/none/parity deb13
check "ssh failure" 2 "ssh to .* failed" -- ping --ssh-option Port=1 deb13
check "option-like target" 2 "must not start with '-'" -- ping -- -oProxyCommand=x
docker exec "$tag-deb13" sh -c 'printf "#!/bin/sh\necho \"PARITY-HOLD 99 9.9.9 \$\$ 2f\"\n" >/usr/local/bin/parity-old && chmod 755 /usr/local/bin/parity-old'
check "version mismatch before password" 2 "remote parity 9.9.9 \(protocol 99\)" -- ping --remote-path /usr/local/bin/parity-old deb13
docker exec "$tag-deb13" rm /usr/local/bin/parity-old
docker exec "$tag-deb13" sh -c 'mkdir -m 777 /opt/open && cp /usr/local/bin/parity /opt/open/'
check "binary writable by others" 2 "can be changed by other users" -- ping --remote-path /opt/open/parity deb13
docker exec "$tag-deb13" sh -c 'chown 1001 /opt/open/parity && chmod 755 /opt/open'
check "binary owned by another user" 2 "can be changed by other users" -- ping --remote-path /opt/open/parity deb13
docker exec "$tag-deb13" rm -r /opt/open
leftovers

check "packages" 0 "^ +bash +amd64" --password testerpw -- packages deb13
check "compare deb9 deb13" 1 "^Packages: [0-9]+ differences" --password testerpw -- compare deb9 deb13
check "content diff" 1 "^ +\+\+\+ deb13:/etc/adduser.conf" --password testerpw -- compare --check config deb9 deb13
check "sensitive skipped" 1 "/etc/shadow: sensitive path" --password testerpw -- compare --check config deb9 deb13
check "identical config" 0 "Result: identical" --password testerpw -- compare "${docker_excludes[@]}" deb13 deb13b
check "identical filesystem" 0 '"result": "identical"' --password testerpw -- compare --check filesystem --format json "${docker_excludes[@]}" deb13 deb13b

docker exec "$tag-deb13b" sh -c 'echo extra >>/etc/adduser.conf; chmod 4755 /usr/bin/ls; mkdir -p /opt/app/lib; touch /opt/app/lib/x; chown 1000 /var/lib/dpkg/info/bash.list'
check "config change" 1 "^ +\+extra" --password testerpw -- compare --check config "${docker_excludes[@]}" deb13 deb13b
check "filesystem mode change" 1 "mode: 0755 -> 4755" --password testerpw -- compare --check filesystem "${docker_excludes[@]}" deb13 deb13b
check "filesystem new tree" 1 "only on deb13b +directory +app \(and 2 entries below\)" --password testerpw -- compare --check filesystem "${docker_excludes[@]}" deb13 deb13b
check "filesystem owner change" 1 "uid: 0 -> 1000" --password testerpw -- compare --check filesystem "${docker_excludes[@]}" deb13 deb13b

# Reading never changes access times on the target.
files="/etc /etc/adduser.conf /etc/debian_version /usr/share/doc /var/lib/dpkg/status"
docker exec "$tag-deb13" sh -c "touch -a -d 2020-01-01 $files"
before=$(docker exec "$tag-deb13" stat -c '%X %n' $files)
check "compare for atime" 1 "Result" --password testerpw -- compare deb9 deb13
after=$(docker exec "$tag-deb13" stat -c '%X %n' $files)
if [[ $before != "$after" ]]; then
    echo "FAIL access times changed:"; diff <(echo "$before") <(echo "$after") || true
    failures=$((failures + 1))
else
    echo "ok   access times unchanged"
fi

# Ctrl-C in the middle of hashing a large file ends everything on the targets.
for c in deb13 deb13b; do docker exec "$tag-$c" sh -c 'head -c 3000000000 /dev/zero >/opt/big'; done
check "interrupt" 130 "" --password testerpw --interrupt-after 4 -- compare --check filesystem --max-hash-size 4G deb13 deb13b
sleep 1
leftovers
for c in deb13 deb13b; do docker exec "$tag-$c" rm -f /opt/big; done

if [[ $failures != 0 ]]; then
    echo "$failures end-to-end test(s) failed"
    exit 1
fi
echo "all end-to-end tests passed"
