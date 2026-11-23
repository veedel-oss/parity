# Parity: Architecture and decisions

## Purpose

Parity compares two Linux machines (installed packages, configuration files, the file system) and reports where they differ. Typical uses: finding configuration drift in clusters, and comparing a legacy or manually set up system with a fresh install of the same release (for example Debian 12) to support infrastructure as code for existing machines. Parity runs on every Linux system that can be reached over SSH. Package collection supports Debian first.

## Core design principle

Parity is a single binary with two roles. On the workstation it is the CLI: it needs no special privileges, connects to the targets over SSH, compares and reports. On each target the CLI starts the same binary over SSH in collect mode, for one run only. Collect mode runs as root, is strictly read-only, only collects raw data (package states, paths, metadata, hashes, file contents where requested) and sends it back. All analysis, meaning the comparison and the evaluation of results, lives in the CLI. This keeps the code that runs as root small and lets the analysis grow without touching the collector.

Root privileges exist on a target only while one run lasts, and they are granted by the target's own sudo or su, not by Parity.

## Components

There is exactly one binary, `parity`. Its roles are subcommands (working names):

- `parity compare <target> <target>`: the CLI, run by the user on the workstation.
- `parity collect`: collect mode, started by the CLI over SSH on a target. Not meant to be run by hand.

Internally the binary is built from separate crates (Rust libraries, not separate programs) with clear boundaries:

- `parity`: the binary crate. Decides the role and hands over. If the first argument is `collect`, it calls into `parity-collect` before any other argument parsing, so no CLI code runs as root.
- `parity-collect`: library for collect mode. Runs on the target as root, reads requests from stdin, answers on stdout, writes logs to stderr and exits. Read-only, no network, no SSH, no analysis.
- `parity-analyze`: library for the CLI side. Starts SSH, handles privilege escalation, speaks the protocol, compares and produces reports.
- `parity-proto`: library for the protocol shared by both sides: request and response types, version, framing, errors.

Boundary rules: `parity-collect` and `parity-analyze` never depend on each other; both depend only on `parity-proto` for what they share. `parity-collect` keeps its dependency list short, because it runs as root. Every dependency there needs a reason.

## Connection model

The CLI uses the system's `ssh` binary, so the user's keys, agent, password, ssh config and jump hosts apply. Authentication, encryption and host verification come from SSH. For each target it runs, roughly, `ssh [options] -- <target> sudo parity collect`. Collect mode handles one session: it reads requests until the end of input or an explicit end request, answers each, and exits. It also exits when writing to stdout fails, so it never outlives the SSH session. No SSH port forwarding is needed. This fails only if the SSH user is restricted to fixed commands (for example by ForceCommand).

Privilege escalation (working name of the option `--become`):

- `sudo` is the default.
- `su` with a password prompt, for systems without sudo (su is part of every Debian installation).
- `none` for when the SSH user is already root.

The password never passes through Parity. Parity has no password option, never reads it, never stores it and never relays it. The prompt from sudo or su goes to the user's terminal and the user types the password there. When two targets need a password, the CLI handles the prompts one after another, then collects from both in parallel. See open points for how the prompt and the protocol share one SSH session.

Remote path: the CLI runs `parity` from `PATH` by default and has an option for a different remote path (working name `--remote-path`). sudo uses its own `secure_path` (on Debian including `/usr/local/bin`), `su -` resets `PATH`, so installing into `/usr/local/bin` or `/usr/bin` works in all cases. If the remote binary is not found, the CLI says so and points to that option. The CLI has to detect this per escalation method: a missing command gives exit code 127 without escalation, but sudo reports it with its own message and exit code 1.

Version check: the first message of every session carries the protocol version. If the versions of the CLI and the remote binary do not match, the CLI stops with a clear error naming both versions.

Target address format: whatever `ssh` accepts as a destination, passed on unchanged: `host` or `user@host` (where `host` may be an alias from `~/.ssh/config`), or `ssh://[user@]host[:port]` for a custom port (IPv6 as `ssh://[fe80::1]:2222`). Users, ports and jump hosts otherwise come from the ssh config. Parity has no address syntax of its own, which avoids the IPv6 ambiguity of `host:port`. A target starting with `-` is rejected, and the CLI always places `--` before the target, so a target can never be read as an ssh option (for example `-oProxyCommand=...`).

## Order of features

1. Installed packages.
2. `/etc` and other configuration directories (for example `/usr/local/etc`), including a comparison of file contents.
3. The whole file system.

## Decisions

- One binary for both roles. The CLI starts collect mode over SSH for one run. Reasons: root privileges exist only for one run, nothing needs to be set up or configured on the targets apart from installing the binary, and authorisation comes from the target's existing sudo or su setup.
- Collect and analysis code stay in separate crates (see Components), so the part that runs as root stays small and reviewable.
- Collect mode refuses to run unless it runs as root (effective UID 0), with a clear error message. The CLI role needs no privileges.
- Only users who can become root on a target (through sudo or su) can inspect it with Parity. Whoever can do that has read access to the whole file system anyway, Parity adds no new access.
- Parity itself leaves no trace on the inspected system. Collect mode opens every file and directory with `O_NOATIME`, so reading does not change access times (with plain reads, `relatime` would still update them about once a day; `stat` never touches them). It writes no files: no state, no cache, no temporary files. Logs go to stderr and reach the CLI. sudo and su log the escalation as usual, that is the target's own audit trail and intended.
- Sensitive paths (for example `/etc/shadow`, `/etc/gshadow`, `/etc/ssh/ssh_host_*_key`, `/etc/ssl/private`, `/root/.ssh`, `/home/*/.ssh`) never leak information about their content by default. Collect mode reports only existence, type, mode, owner and group for them. Size, modification time, hashes and content are withheld, because a size can reveal the length of a secret and a hash of a small file can be guessed. The list is built in. The user running the CLI (who is root-capable on the target anyway) can add paths with an explicit option and can opt in to metadata and hashes for sensitive paths with an explicit option. The content of sensitive files is never transferred. The default protects reports and terminals from secrets, it is not a security boundary against the root-capable user.
- Collect mode serves file contents on request, for the content comparison of configuration files. The CLI fetches contents only for files whose hashes differ, within a size limit.
- Parity runs on every Linux system that can be reached over SSH, old and new (for example Debian 9 to Debian 13, on the target as well as on the workstation). Therefore it is written in Rust and statically linked against musl: it needs no libc or other libraries on the target, and the same binary runs everywhere. A target needs the `parity` binary, an SSH server and sudo or su. The remaining limits are the minimum kernel of the Rust target (currently 3.2), one build per CPU architecture, and package collection, which needs support per distribution.
- Collect mode uses newer system calls only with a fallback, so it keeps working on old kernels (for example `statx` from 4.11, `openat2` from 5.6; Debian 9 ships 4.9). Not following symlinks is done with `openat` and `O_NOFOLLOW` step by step, not with `openat2`.
- Collect mode reports owner and group as raw UID and GID and does no name lookups (no NSS, no libc user database). Where names are needed, the CLI maps the numbers itself, using `/etc/passwd` and `/etc/group` from the collected data.
- License: AGPL-3.0-or-later.
- No rsync. An rsync-based comparison depends too much on the network setup (sudo on both targets, SSH port forwarding between them for itemized changes).
- `parity compare` exit codes follow `diff` and `cmp`: 0 targets identical, 1 differences found, 2 error (the comparison could not be completed, for example a target unreachable, `parity` not found on the target, escalation failed, a protocol error). Files skipped on purpose (size limit, sensitive paths, excluded paths) are listed in the report but do not change the exit code.

## Open points

- How the sudo or su prompt and the protocol share one SSH session without the password passing through Parity. A prompt needs a terminal on the target, but stdin and stdout of the session carry the protocol. To be settled by a prototype before the transport is built (see `docs/plan.md`).
- How the binary gets onto the target: installed beforehand (current assumption) or copied for one run (which would write to the target).
- Which CPU architectures get a build, and how the right binary for a target is chosen.
- Final command line options and option names.
- Wire protocol and data format.
- How to collect file hashes efficiently on large file systems.
- Package state collection beyond Debian.
- The binary name `parity` may clash with other tools of that name; check before the first release.
