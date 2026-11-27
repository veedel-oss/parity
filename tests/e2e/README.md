# e2e tests

`run.sh OS` tests what only a real system can show: an own kernel, real ssh, sudo and su. The OS is one of `debian-10`, `debian-11`, `debian-12` or `debian-13`; nothing else is tested. CI runs all four, one job per OS (`.github/workflows/e2e.yml`). How to run them locally is in [CONTRIBUTING.md](../../CONTRIBUTING.md#tests).

## How they run

Each run boots two VMs, `vm1` and `vm2`, from the latest genericcloud image of the release from cloud.debian.org: the `latest` folder, without a checksum check. The image is cached in `~/.cache/parity-vm` and downloaded again when the server has a newer one. qemu runs in a container built from the [Dockerfile](Dockerfile) here.

The static binary is installed in `/usr/local/bin` on both VMs and tested over real ssh, with sudo, with su and as root.

## What they cover

- the workspace tests, run as root on each VM
- `ping`, `packages` and `compare` with sudo, with su and as root
- escalation and connection errors, and the checks of the installed binary
- two fresh installs compare as identical, and changes made to one show up
- dpkg's journal
- access times on ext4
- no process left behind after errors and after Ctrl-C while hashing a 64 GB file

Older kernels down to Linux 3.2 are kept working by using only their system calls ([collect-mode.md](../../docs/collect-mode.md#minimum-kernel)), not by tests. There are no container tests: containers share the host kernel, so VMs cover everything they would.
