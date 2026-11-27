# Runs packaging/deb/build.sh for `make build-deb`, so the Debian package
# can be built on a host without dpkg-deb. The base image already has all
# the script needs (bash, coreutils, findutils, gzip and dpkg-deb), so
# nothing is installed.
# The base image is pinned by digest, so a new one comes only with a
# reviewed change: Dependabot proposes the new digest when the trixie tag
# moves (.github/dependabot.yml).
FROM debian:trixie@sha256:3cda3ca472ce45507a5109c6a3907093e24d9ae67d55954c9a07a45665dafba2
