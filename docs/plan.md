# Plan

Working plan for the Rust implementation. Work on one or two steps per session, add tests, then tick the step off. Keep this file up to date after every step. Architecture and decisions live in `docs/architecture.md`.

Milestones 1 (end-to-end skeleton), 2 (comparison of packages), 3 (comparison of configuration directories) and 4 (comparison of the whole file system) are complete and removed; their decisions are in `docs/architecture.md`.

## Milestone 5: Release preparation

Steps 21 to 23 are complete; their decisions are in `docs/architecture.md` (Decisions, Command line, Builds) and `docs/security-review.md`. Steps 25 to 29 make sure that only the maintainer can publish a release, and that a download can be checked against the maintainer's key.

- [ ] 24. First tagged release, 0.0.1.
  - Done: name (`parity`, not on crates.io), release notes in `CHANGELOG.md`, `.github/workflows/release.yml`.
  - Open: steps 25 to 29 first. Then, on the release commit, CI and e2e pass, the maintainer pushes the signed tag `v0.0.1` and publishes the draft release.
- [x] 25. Rulesets, setup GitHub Rulesets
- [x] 26. Repository and organisation settings, set by the maintainer: the maintainer is the only organisation owner, the base permission gives members no write access, nobody else has write, maintain or admin access to the repository, two-factor authentication is required, Actions allows only actions created by GitHub and requires full-length commit SHAs, the default `GITHUB_TOKEN` is read-only and cannot create or approve pull requests, workflows from fork pull requests of external contributors need approval, releases are immutable, and Dependabot alerts and security updates, Secret Protection (secret scanning and push protection) and private vulnerability reporting are enabled. The workflows fit these settings: only `actions/*`, each pinned to a full commit SHA, and write access only in the release job.
- [x] 27. `release.yml` in two jobs: a build job with read-only permissions and `persist-credentials: false`, and a release job with write permissions that runs no code of the project or its dependencies and creates the release as a draft. (Since the repository is public: the release job runs in the environment `release`, set up by the maintainer, and attests the build provenance of the binary with `actions/attest`. See `docs/architecture.md`, Builds.)
- [ ] 28. Pin the Rust toolchain of the release build, so it changes only with a commit.
- [ ] 29. Signed checksums: the maintainer signs `SHA256SUMS` with their GPG key and attaches `SHA256SUMS.asc` to the draft before publishing it. The README and the release notes say how to verify a download.

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
