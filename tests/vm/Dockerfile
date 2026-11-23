# Runs qemu for tests/vm/run.sh, so nothing needs to be installed on the host
# apart from docker. Needs /dev/kvm passed in.
FROM debian:trixie
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        qemu-system-x86 qemu-utils genisoimage \
    && rm -rf /var/lib/apt/lists/*
