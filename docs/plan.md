# Plan

Working plan for the Rust implementation. Work on one or two steps per session, add tests, then tick the step off. Keep this file up to date after every step. Architecture and decisions live in `docs/architecture.md`.

Milestones 1 (end-to-end skeleton), 2 (comparison of packages), 3 (comparison of configuration directories) and 4 (comparison of the whole file system) are complete and removed; their decisions are in `docs/architecture.md`.

## Milestone 5: Release preparation

- [x] 21. Release builds for the chosen CPU architectures, size check of the binary. (x86_64 and aarch64 musl, about 1.2 MB and 1.0 MB, CI limit 5 MB. Which architectures get released is still open. The aarch64 binary is built but not yet run anywhere.)
- [ ] 22. Finalise command and option names and update the README (they are placeholders today).
- [x] 23. Security review of collect mode: input validation, resource limits, behaviour with symlinks and special files, no write path anywhere, no changed access times (`O_NOATIME` everywhere, test that atime stays unchanged), the sensitive-path list and that no request can bypass it, no CLI code reachable as root, no remote process left behind after an interrupted run, a test run on the oldest supported kernel.
  (Review in `docs/security-review.md`, with the fixes it led to and tests; system tests in `tests/vm/run.sh`.)
- [ ] 24. Check that the crate names and the binary name are free (crates.io, common distributions), release notes, first tagged release.
  - Done: name check (see `docs/architecture.md`, open points: `parity` is taken on crates.io), draft release notes in `CHANGELOG.md`.
  - Open: decide on the name, tag the release.

## Later

- Package collection for more distributions beyond Debian.
- Map UIDs and GIDs to names in the CLI from the collected `/etc/passwd` and `/etc/group`, so system users with different UIDs on two machines do not show up as differences.
- Comparison against a saved snapshot instead of two live machines.
- Export of differences as Ansible or Terraform code.
- Smarter hashing on very large file systems (hash only files whose size matches on both sides, or in a second pass).
- Run the aarch64 build in CI on an arm runner.

## Open questions

- Whether the CLI may copy the binary to the target for one run, or the binary must be installed beforehand.
- Which CPU architectures get a release build, and how the right binary for a target is chosen.
- The name: `parity` is taken on crates.io.
