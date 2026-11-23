# AGENTS.md

Guidance for automated coding agents and contributors working in this repository.

## Project

Parity compares two Linux machines and reports differences in files, configuration and installed packages. Written in Rust, statically linked against musl, runs on every Linux system reachable over SSH. AGPL-3.0-or-later. Part of the GitHub organisation veedel-oss. Read `docs/architecture.md` first.

## Core principles

- Parity is a single binary. On the workstation it is the CLI, on a target the CLI starts it over SSH in collect mode for one run.
- Collect mode stays minimal: it runs as root, is read-only, reads requests from stdin, answers on stdout and exits. It only collects and sends raw data. Never add analysis logic, writing, or configuration changes to it.
- All analysis lives in the CLI side, which runs without special privileges.
- All communication runs over the SSH session (stdin and stdout). Do not require SSH port forwarding.
- Privilege escalation on the target uses sudo by default or su. The password never passes through Parity: no password option, never read, stored or relayed by Parity. The prompt runs over the terminal.
- Parity never changes the systems it inspects.

## Workspace layout

One binary, `parity`, with the subcommands `compare` (CLI) and `collect` (collect mode). The other crates are libraries linked into it, not separate programs.

- `crates/parity`: the binary, dispatches to collect mode or the CLI
- `crates/parity-collect`: collect mode, runs as root on the target
- `crates/parity-analyze`: CLI side: SSH, escalation, comparison, reports
- `crates/parity-proto`: shared protocol types

`parity-collect` and `parity-analyze` never depend on each other. The binary hands over to `parity-collect` before any other argument parsing, so no CLI code runs as root.

## Conventions

- Keep the dependency list of `parity-collect` short. Every dependency that runs as root needs a reason.
- Collect mode must run on old kernels (down to the Rust minimum, currently 3.2): use newer system calls only with a fallback.
- Collect mode does no name lookups. It sends UIDs and GIDs as numbers; mapping to names happens in the CLI.
- Prefer calling the system `ssh` binary over embedding an SSH library, so the user's ssh config and agent apply.
- No company-specific data, host names, IPs or configuration anywhere in the repository, including tests and git history.
- All code is written from scratch for this repository.
- Tests never depend on the system that runs them. Unit tests are pure: pass in what they need (paths, UIDs, metadata, file system types) or mock it. Integration tests in `crates/*/tests` only touch fixtures they create; collect mode takes the paths it reads on its own (dpkg database, built-in sensitive list) from `session::Config`. What only a real system can show (kernel behaviour, access times, `/proc`) belongs in `tests/e2e` and `tests/vm`.
- Command and option names in the README are still placeholders. Ask before fixing them.
- Tick off steps in `docs/plan.md` as they are done. When a milestone is complete, delete it from the plan and move any lasting decisions into `docs/architecture.md`.

## Status

The implementation is at the start. The architecture is decided, the protocol, the details of privilege escalation and the command line options are not.