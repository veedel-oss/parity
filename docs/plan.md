# Plan

Working plan for the Rust implementation. Work on one or two steps per session, add tests, then tick the step off. Keep this file up to date after every step. Architecture and decisions live in `docs/architecture.md`.

## Milestone 1: End-to-end skeleton

Goal: one request travels the whole chain, from the CLI over SSH and sudo to collect mode on the target and back.

- [x] 1. `LICENSE`, `.gitignore`, a CI workflow for build, tests, `cargo fmt` and `cargo clippy`.
- [x] 2. Set up the Cargo workspace with the crates `parity` (binary), `parity-collect`, `parity-analyze` and `parity-proto` (see `docs/architecture.md`). Add a CI check that `parity-collect` and `parity-analyze` do not depend on each other.
- [ ] 3. Static build for `x86_64-unknown-linux-musl` and a CI job for it. Confirm the minimum kernel of the Rust target. Check that the binary starts on an old and a new target (for example Debian 9 and Debian 13 VMs; containers are not enough, they share the host kernel).
- [ ] 4. Protocol in `parity-proto`: request and response types, a version field sent first in every session, framing of messages over a byte stream, an end-of-session request, errors. Unit tests for encoding and decoding.
- [ ] 5. Collect mode in `parity-collect`: the binary dispatches the collect subcommand before any other argument parsing. Refuse to run unless running as root. Read requests from stdin, answer on stdout, logs to stderr, exit on end of input, on the end request and on write errors. Answer a simple ping request. Tests over pipes, with the root check replaceable in tests.
- [ ] 6. Prototype of privilege escalation: sudo (default) and su with a password prompt on the terminal, password never passing through Parity, protocol on stdin and stdout of the same SSH session. Try the candidate approaches against real Debian targets, decide, and record the decision in `docs/architecture.md`.
- [ ] 7. CLI transport in `parity-analyze`: call the system `ssh` binary and start collect mode with the chosen escalation (working name `--become sudo|su|none`, default `sudo`). Target format: an ssh destination passed on unchanged (`[user@]host` or `ssh://[user@]host[:port]`), reject targets starting with `-`, always `--` before the target. Tests for the rejection. Option for the remote path (working name `--remote-path`, default `parity` from `PATH`). Version check on connect. Password prompts for several targets one after another. Clear error messages for SSH failures, binary not found on the target (detected per escalation method, hint to the option), escalation refused or wrong password, version mismatch. Interrupting the CLI ends the remote process.
- [ ] 8. First real request: list of installed Debian packages with versions and architecture, read from the dpkg database. Collect mode collects, CLI prints.

## Milestone 2: Comparison of packages

- [ ] 9. `parity compare`: fetch package lists from two targets and report differences (missing, extra, different version).
- [ ] 10. Output formats: readable table for people, JSON for scripts.
- [ ] 11. Exit codes: 0 identical, 1 different, 2 error; intentionally skipped files do not change the exit code (see `docs/architecture.md`).

## Milestone 3: Comparison of configuration directories

- [ ] 12. Request for file metadata below given roots: path, type, mode, owner and group as raw UID and GID, size, modification time, link target. Default roots `/etc` and further configuration directories (for example `/usr/local/etc`), configurable in the CLI. Files and directories are opened with `O_NOATIME`, symlinks are never followed (`openat` with `O_NOFOLLOW`, no `openat2`, so it works on old kernels).
- [ ] 13. Sensitive paths: built-in list, only existence, type, mode, owner and group by default. CLI options to add paths and to opt in to metadata and hashes. Content of sensitive files is never sent. Tests that no request can get around this.
- [ ] 14. Content hashes for regular files, streamed in chunks, with a size limit and a clear way to skip large files.
- [ ] 15. Request for file contents: the CLI fetches contents only for files whose hashes differ, within a size limit.
- [ ] 16. `parity compare` for configuration directories: differences in metadata, and a unified diff for text files whose content differs. Binary files are reported as different without a diff.

## Milestone 4: Comparison of the whole file system

- [ ] 17. Include and exclude rules for paths (for example `/proc`, `/sys`, `/run`, caches), configurable in the CLI and sent with the request.
- [ ] 18. Collect mode streams results in batches so large file systems do not exhaust memory.
- [ ] 19. `parity compare` for the whole file system: differences in metadata and hashes, grouped by directory.
- [ ] 20. Comparison against a base installation: compare a legacy or manually set up system with a fresh install of the same release (for example a VM), to show everything that was added or changed on top. Document this workflow and test it.

## Milestone 5: Release preparation

- [ ] 21. Release builds for the chosen CPU architectures, size check of the binary.
- [ ] 22. Finalise command and option names and update the README (they are placeholders today).
- [ ] 23. Security review of collect mode: input validation, resource limits, behaviour with symlinks and special files, no write path anywhere, no changed access times (`O_NOATIME` everywhere, test that atime stays unchanged), the sensitive-path list and that no request can bypass it, no CLI code reachable as root, no remote process left behind after an interrupted run, a test run on the oldest supported kernel.
- [ ] 24. Check that the crate names and the binary name are free (crates.io, common distributions), release notes, first tagged release.

## Later

- Package collection for more distributions beyond Debian.
- Map UIDs and GIDs to names in the CLI from the collected `/etc/passwd` and `/etc/group`, so system users with different UIDs on two machines do not show up as differences.
- Comparison against a saved snapshot instead of two live machines.
- Export of differences as Ansible or Terraform code.

## Open questions

- Serialisation format for the protocol (for example length-prefixed messages with a compact binary format or JSON lines).
- How the sudo or su prompt and the protocol share one SSH session (step 6).
- Whether the CLI may copy the binary to the target for one run, or the binary must be installed beforehand.
- Strategy for hashing on very large file systems.
- Which CPU architectures get a build, and how the right binary for a target is chosen.
