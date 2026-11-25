#!/usr/bin/env bash
#
# packaging/deb/build.sh BINARY OUTDIR
#
# Builds the Debian package parity_<version>_amd64.deb in OUTDIR from the
# static parity binary BINARY, as the release does
# (.github/workflows/release.yml), and prints its path. The package holds
# that binary as it is, as /usr/bin/parity, the changelog and the copyright
# file; its version is the one the binary reports. All file times are
# SOURCE_DATE_EPOCH, by default the time of the commit, so the same commit
# gives the same package.
#
# Needs dpkg-deb, which comes with dpkg.

set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 BINARY OUTDIR" >&2
    exit 2
fi
bin=$1
out=$2
root=$(cd "$(dirname "$0")/../.." && pwd)

version=$("${bin}" --version)
version=${version#parity }
if ! [[ ${version} =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "$0: ${bin} reports no version 'parity X.Y.Z'" >&2
    exit 1
fi
if [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
    SOURCE_DATE_EPOCH=$(git -C "${root}" log -1 --format=%ct)
fi
# dpkg-deb takes it for the times in the ar archive.
export SOURCE_DATE_EPOCH

tree=$(mktemp -d)
trap 'rm -rf "$tree"' EXIT
umask 022
chmod 0755 "${tree}"

install -D -m 0755 "${bin}" "${tree}/usr/bin/parity"
doc=${tree}/usr/share/doc/parity
install -d "${doc}"
gzip -9n <"${root}/CHANGELOG.md" >"${doc}/changelog.gz"
install -m 0644 "${root}/packaging/deb/copyright" "${doc}/copyright"

install -d "${tree}/DEBIAN"
(cd "${tree}" && find usr -type f -print0 | sort -z | xargs -0 md5sum) >"${tree}/DEBIAN/md5sums"
# No Depends or Recommends: the binary is static, and the workstation needs
# an ssh client while a target needs an SSH server and sudo or su, which one
# package for both roles cannot express. No maintainer scripts, conffiles or
# services: installing configures nothing, and removing leaves nothing behind.
# The version has no Debian revision, since releases are immutable.
cat >"${tree}/DEBIAN/control" <<EOF
Package: parity
Version: ${version}
Architecture: amd64
Maintainer: veedel-oss <veedel-oss@mailbox.org>
Section: admin
Priority: optional
Homepage: https://github.com/veedel-oss/parity
Description: Compare two Linux machines and show where they differ
 Parity compares two Linux machines over SSH and shows where they differ:
 installed Debian packages, configuration directories such as /etc including
 the content of the files, and the whole file system.
 .
 The same static binary is the CLI on the workstation and the collector on
 each target, which the CLI starts over SSH with sudo or su for one run. The
 collector only reads: Parity never changes the systems it inspects.
EOF
find "${tree}" -exec touch --no-dereference --date="@${SOURCE_DATE_EPOCH}" {} +

# xz, because dpkg before 1.21.18 (Debian 10 and 11) cannot unpack zstd.
mkdir -p "${out}"
dpkg-deb --root-owner-group -Zxz --build "${tree}" "${out}/parity_${version}_amd64.deb" >/dev/null
echo "${out}/parity_${version}_amd64.deb"
