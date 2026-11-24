# Security policy

## Supported versions

Only the latest release gets security fixes. Parity is before 1.0, so a fix comes as a new release, not as a backport to an older one.

## Reporting a vulnerability

Please do not report security problems in public issues, pull requests or discussions. Report them privately:

- with GitHub's private vulnerability reporting: [Report a vulnerability](https://github.com/veedel-oss/parity/security/advisories/new),
- or by email to veedel-oss@mailbox.org.

Please include:

- the affected version (`parity --version`)
- the distribution and kernel of the workstation and of the target
- how to reproduce the problem
- who the attacker is (another user on the target, the SSH user, someone on the network, the user running the CLI) and what they gain

You will get a confirmation that your report arrived. The problem is fixed in a new release, and an advisory is published together with it, crediting you if you wish. Please keep the problem private until then.

## Security model

This helps to judge whether something is a vulnerability. The details are in [docs/architecture.md](docs/architecture.md), the review of collect mode with its known limits in [docs/security-review.md](docs/security-review.md).

- Collect mode runs as root on the target, for one run, started through the target's own sudo or su. It only reads: it writes no files, leaves access times unchanged, never follows symlinks, never reads pseudo file systems such as `/proc`, and exits when the SSH session ends. Anything that makes it write, change the target, follow a symlink, block, or stay behind is a vulnerability.
- No code of the CLI runs as root.
- The password for sudo or su never passes through Parity: the user types it on their own terminal. Anything that exposes it to Parity is a vulnerability.
- Only a user who can become root on a target can inspect it. A way for anyone else to gain root, to take over a session, to replace the binary that is about to run as root, or to send requests to collect mode is a vulnerability.
- Authentication, encryption and host verification come from the system's `ssh` and the user's ssh configuration. Parity adds no network service and needs no port forwarding.
- The content of sensitive paths (for example `/etc/shadow` and SSH host keys) is never sent, and by default their size, modification time and hash are withheld too. A way to get the content of a sensitive file out of collect mode is a vulnerability. The list protects reports and terminals. It is not a boundary against the user running Parity, who can become root on the target anyway.

Not vulnerabilities: what the user who can become root on the target can see by design, and the known limits listed in [docs/security-review.md](docs/security-review.md).

## Verifying a release

Every release binary comes with a `SHA256SUMS` file and a build provenance attestation, which shows that the binary was built from the tagged commit by the release workflow of this repository:

```bash
sha256sum -c SHA256SUMS
gh attestation verify parity-<version>-x86_64-unknown-linux-musl --repo veedel-oss/parity
```
