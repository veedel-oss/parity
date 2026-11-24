# Plan

Working plan for the Rust implementation. Work on one or two steps per session, add tests, then tick the step off. Keep this file up to date after every step. Architecture and decisions live in `docs/architecture.md`.

Milestones 1 (end-to-end skeleton), 2 (comparison of packages), 3 (comparison of configuration directories), 4 (comparison of the whole file system) and 5 (release preparation, first release 0.0.1) are complete and removed; their decisions are in `docs/architecture.md`.

## Later

- Package collection for more distributions beyond Debian.
- Map UIDs and GIDs to names in the CLI from the collected `/etc/passwd` and `/etc/group`, so system users with different UIDs on two machines do not show up as differences.
- Comparison against a saved snapshot instead of two live machines.
- Export of differences as Ansible or Terraform code.
- Smarter hashing on very large file systems (hash only files whose size matches on both sides, or in a second pass).
- Run the aarch64 build in CI on an arm runner, then release it.

## Open questions

- Whether the CLI may copy the binary to the target for one run, or the binary must be installed beforehand.
- Whether the matching of path patterns for sensitive paths and excludes (`*` within one component, `crates/parity-collect/src/pattern.rs`) should also come from a well-known library such as `globset`, following the rule in `AGENTS.md` that security-relevant algorithms are never our own. It is the last such piece written here; against it speaks the size of that dependency in collect mode, which runs as root.
