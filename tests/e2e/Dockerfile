# Runs qemu for tests/e2e/run.sh, so nothing needs to be installed on the host
# apart from docker. Needs /dev/kvm passed in.
# The base image is pinned by digest, so a new one comes only with a
# reviewed change: Dependabot proposes the new digest when the trixie tag
# moves (.github/dependabot.yml). The packages below still come from the
# Debian archive when the image is built.
FROM debian:trixie@sha256:3cda3ca472ce45507a5109c6a3907093e24d9ae67d55954c9a07a45665dafba2
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        qemu-system-x86 qemu-utils genisoimage \
    && rm -rf /var/lib/apt/lists/*
