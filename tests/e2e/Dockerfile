# Runs qemu for tests/e2e/run.sh, so the host needs only docker and /dev/kvm.
FROM debian:trixie@sha256:3cda3ca472ce45507a5109c6a3907093e24d9ae67d55954c9a07a45665dafba2

# hadolint ignore=DL3008 # the archive keeps only the current versions
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        qemu-system-x86 \
        qemu-utils \
        genisoimage \
    && rm -rf /var/lib/apt/lists/*
