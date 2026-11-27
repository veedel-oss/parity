# Parity: Architecture and decisions

## Purpose

Parity compares two Linux machines (installed packages, configuration files, the file system) and reports where they differ. Typical uses: finding configuration drift in clusters, and comparing a legacy or manually set up system with a fresh install of the same release (for example Debian 12) to support infrastructure as code for existing machines. Parity runs on every Linux system that can be reached over SSH. Package collection supports Debian first.

## Core design principle

Parity is a single binary with two roles. On the workstation it is the CLI: it needs no special privileges, connects to the targets over SSH, compares and reports. On each target the CLI starts the same binary over SSH in collect mode, for one run only. Collect mode runs as root, is strictly read-only, only collects raw data (package states, paths, metadata, hashes, file contents where requested) and sends it back. All analysis, meaning the comparison and the evaluation of results, lives in the CLI. This keeps the code that runs as root small and lets the analysis grow without touching the collector.

Root privileges exist on a target only while one run lasts, and they are granted by the target's own sudo or su, not by Parity.

## Components

There is exactly one binary, `parity`. Its roles are subcommands:

- `parity compare <target> <target>`: the CLI, run by the user on the workstation.
- `parity collect`: collect mode, started by the CLI over SSH on a target. Not meant to be run by hand.
- `parity hold <nonce>`: unprivileged helper on the target for escalated sessions (see Connection model). Not meant to be run by hand.

The CLI also has `parity packages <target>` (list installed packages), `parity ping <target>` (check that collect mode starts) and `parity completion <shell>` (print the shell completion script). The commands and options are listed under Command line.

Internally the binary is built from separate crates (Rust libraries, not separate programs) with clear boundaries:

- `parity`: the binary crate. Decides the role and hands over. If the first argument is `collect` or `hold`, it calls into `parity-collect` before any other argument parsing, so no CLI code runs as root.
- `parity-collect`: library for collect mode. Runs on the target as root, reads requests from stdin, answers on stdout, writes logs to stderr and exits. Read-only, no network, no SSH, no analysis.
- `parity-analyze`: library for the CLI side. Starts SSH, handles privilege escalation, speaks the protocol, compares and produces reports.
- `parity-proto`: library for the protocol shared by both sides: request and response types, version, framing, errors.

Boundary rules: `parity-collect` and `parity-analyze` never depend on each other; both depend only on `parity-proto` for what they share. `parity-collect` keeps its dependency list short, because it runs as root. Every dependency there needs a reason.

## Connection model

The CLI runs the system's `ssh`, so the user's keys, agent, ssh config and jump hosts apply, and SSH provides authentication, encryption and host verification. Collect mode serves one session over the stdin and stdout of ssh and exits when it ends. No port forwarding is needed. It does not work for an SSH user restricted to fixed commands (ForceCommand).

### Escalation

`--become` chooses how collect mode becomes root:

| `--become` | For |
| --- | --- |
| `sudo` (default) | an SSH user who may use sudo |
| `su` | systems without sudo; su asks for root's password |
| `none` | an SSH user who is root |

The password never passes through Parity. There is no password option, and the prompt of sudo or su appears on the user's terminal, where the user types the password. With two targets the prompts come one after the other, then both are collected in parallel.

### Two sessions over one connection

With `--become none`, one session runs `parity collect`. With sudo or su, the prompt needs a terminal, but the protocol must not pass through one. So the CLI opens two sessions over one SSH connection, with OpenSSH connection multiplexing and a private control socket on the workstation:

```
workstation                           target
                 protocol session
CLI  ── ssh -T ─────────────────────▶ parity hold <nonce>            SSH user
     ◀─────────── hold line ────────  keeps its pipes open
                 escalation session
     ── ssh -tt ────────────────────▶ sudo parity collect --attach   root
terminal ◀──── password prompt ─────  takes over the holder's pipes,
                                      detaches, sudo returns
CLI  ◀═══════════ protocol ════════▶  collect mode
```

1. The protocol session runs `parity hold <nonce>` as the SSH user. It prints one line with its PID, the protocol version and the path of its binary, and keeps its pipes open.
2. The escalation session runs `sudo` or `su` with `parity collect --attach <pid> <nonce>` on a terminal, so the prompt reaches the user.
3. Collect mode, now root, checks that the holder is the user's own `parity hold` with the same nonce, takes over its pipes and detaches. sudo or su return, and the escalation session ends.
4. The CLI speaks the protocol over the protocol session.

The checks of steps 1 and 3 are in [collect-mode.md](collect-mode.md#attaching).

### Version and path

The hold line carries the protocol version, so a mismatch is reported before anyone is asked for a password. The first message of every session carries it too, in an encoding that never changes (see Protocol).

The CLI runs `parity` from the SSH user's `PATH`, or the path given with `--parity-path`. The escalated command uses the absolute path the holder reports, so sudo's `secure_path` and su's environment do not matter, and the same binary runs in both sessions. A missing binary therefore always shows up in the unprivileged session.

### End of a session

When the session ends or the CLI is interrupted, ssh closes the pipes. Collect mode sees the end of its input or a failed write and exits, also in the middle of a walk, and the holder sees the hangup and exits. Nothing stays behind on the target.

### Targets and errors

A target is whatever `ssh` accepts as a destination, passed on unchanged: `host`, `user@host`, an alias from `~/.ssh/config`, or `ssh://[user@]host[:port]` for a port (IPv6 as `ssh://[fe80::1]:2222`). Parity has no address syntax of its own, which avoids the IPv6 ambiguity of `host:port`. A target starting with `-` is rejected, and the CLI always puts `--` before the target, so it is never read as an ssh option.

Each failure gets its own message:

| Failure | Detected by |
| --- | --- |
| ssh failed | exit code 255 |
| `parity` not found on the target | exit code 126 or 127; the message points to `--parity-path` |
| not root with `--become none` | collect mode refuses |
| escalation refused or wrong password | the escalation session ends with an error; sudo's or su's own message is already on the terminal |
| version mismatch | the hold line or the first message; both versions are named |
| output that is not the protocol | for example from shell startup files |

## Order of features

1. Installed packages.
2. `/etc` and other configuration directories (for example `/usr/local/etc`), including a comparison of file contents.
3. The whole file system.

## Decisions

Each decision with its reason.

### Roles and privileges

- **One binary for both roles.** The CLI starts collect mode over SSH for one run: root privileges exist only for that run, a target needs nothing set up apart from the binary, and authorisation comes from its existing sudo or su setup.
- **Collecting and analysing in separate crates** (see Components), so the code that runs as root stays small and reviewable.
- **Collect mode runs only as root** (effective UID 0) and says so otherwise. The CLI needs no privileges.
- **Only users who can become root on a target can inspect it.** They can read the whole file system anyway, so Parity adds no access.
- **Escalation over two sessions of one SSH connection** (see Connection model). Ruled out:

  | Alternative | Why not |
  | --- | --- |
  | a terminal for the whole session | the protocol would pass through a line discipline, and the CLI would have to relay the password |
  | `sudo -S` | the password would pass through Parity |
  | `sudo -v` in a first session | the sudo timestamp is bound to the terminal, a second session does not share it, and su has no timestamp at all |
  | a socket or FIFO on the target | it would write to the target and open a local channel to a root process |

### Reading the target

- **No trace on the target.** Collect mode writes no files, no state, cache or temporary files, and leaves access times unchanged, which with plain reads `relatime` would still update about once a day (see Collect mode details). Its logs go to stderr and reach the CLI. sudo and su log the escalation as usual: that is the target's own audit trail.
- **Sensitive paths reveal nothing about their content by default.** For paths such as `/etc/shadow`, `/etc/ssh/ssh_host_*_key` or `/home/*/.ssh`, collect mode reports only existence, type, mode, owner and group. Size, modification time, link target, device number, hash and content are withheld: a size can reveal the length of a secret, and a hash can give away a short one, as the content hash is not cryptographic (see Comparison).
- **The user can add sensitive paths and opt in to more metadata,** but the hash and content of a sensitive file are never sent. The built-in list keeps secrets out of reports and terminals; it is no boundary against the root-capable user.
- **File contents only on request,** for the diff of configuration files: the CLI fetches only files whose hashes differ, within a size limit.
- **UIDs and GIDs as numbers.** Collect mode does no name lookups: no NSS, no libc user database.

### Where it runs

- **Every Linux system that can be reached over SSH,** old and new. Supported and tested are Debian 10 to 13, as target and as workstation. So Parity is written in Rust and statically linked against musl: the same binary runs everywhere and needs no libraries on the target. A target needs the binary, an SSH server and sudo or su. What stays limited: the minimum kernel of the Rust target (Linux 3.2), one build per CPU architecture, and package collection, which needs support for each distribution.
- **Newer system calls only with a fallback,** so old kernels keep working: `statx` is from Linux 4.11, `openat2` from 5.6, and Debian 10 ships 4.19. Symlinks are not followed thanks to `openat` with `O_NOFOLLOW` step by step, not `openat2`.
- **x86_64 only,** the architecture the e2e tests run on. Another one is built, tested and released together, once CI can run the e2e tests on it.

### Protocol and dependencies

- **A compact binary protocol of its own,** not JSON lines, so collect mode needs no serialisation library (see Protocol).
- **Collect mode depends only on `parity-proto`, `libc` and `xxhash-rust`** (XXH3-128 for content hashes, see Comparison). Hashes and other security-relevant algorithms come from well-known libraries, never from this repository. The CLI takes the nonces for attaching from `rand`, a ChaCha CSPRNG seeded from the system's random source.
- **clap without its features `color` and `suggestions`.** Help and errors are plain text without "did you mean" hints, which keeps the seven crates for terminal colours and string similarity out of the binary.

### Output, name and distribution

- **Exit codes as with `diff` and `cmp`:** 0 identical, 1 differences found, 2 error, when the comparison could not be completed (a target unreachable, `parity` not found on it, escalation failed, a protocol error). Files skipped on purpose (size limit, sensitive or excluded paths) are listed in the report but do not change the exit code.
- **No rsync.** A comparison with rsync depends too much on the network setup: sudo on both targets, and SSH port forwarding between them for itemized changes.
- **The name stays `parity`, and the crates are not published on crates.io** (`publish = false`). There `parity` is taken by an unrelated crate, the `parity-` prefix is in practice that of Parity Technologies (`parity-scale-codec`, `parity-wasm`, `parity-db`), and `cargo install` would not build the static musl binary a target needs. Releases are on GitHub, with the static binary and a Debian package made from it (see [maintaining.md](maintaining.md#releases)).
- **The Debian package is called `parity` too.** No distribution has a package of that name: Repology lists only `parity-git` in the AUR, the discontinued Parity Ethereum client, whose binary was also called `parity`.
- **License:** AGPL-3.0-or-later.

## Protocol

Implemented in `parity-proto` without dependencies.

- Framing: every message is a frame of a 4-byte big-endian length and the payload. Collect mode accepts request frames up to 1 MiB, the CLI accepts frames up to 16 MiB.
- Encoding: a compact binary format written for Parity. Integers big-endian with fixed width, byte strings and lists prefixed with a u32 length, options and enums with a u8 tag. Decoders check every length against the bytes left, so a forged length cannot make them allocate. Paths are byte strings, Linux paths need not be UTF-8.
- Session: both sides first send a hello (magic `PRTY`, u32 protocol version, program version). Then the CLI sends requests and collect mode answers each in order: `Ping`, `DpkgStatus` (raw `/var/lib/dpkg/status` in chunks, parsed by the CLI), `Walk` (metadata below roots, streamed in batches of at most 1000 entries, errors per path, an end marker), `ReadFile` (content of one regular file in chunks of 256 KiB, within a size limit, at most 64 MiB), and `End`. An undecodable request ends the session.

## Collect mode details

- Every path from the CLI must be absolute and normalised (no empty, `.` or `..` components, no trailing slash, no NUL, at most 4096 bytes). Limits per request: 64 roots, 4096 excludes, 256 extra sensitive patterns.
- Paths are opened step by step from `/` with `openat` and `O_NOFOLLOW`. Directories that are only passed through are opened with `O_PATH` (Linux 2.6.39), which never changes an access time and needs no ownership for `O_NOATIME`. Directories and files that are read are opened with `O_NOATIME`. There is no fallback without it: a file that cannot be opened that way is reported as an error rather than read with a changed access time.
- Files are opened with `O_NONBLOCK` and checked with `fstat` after opening, so a FIFO swapped in after the `stat` cannot block the walk.
- Files on pseudo file systems (proc, sysfs, debugfs, tracefs, securityfs, cgroup, bpf, pstore, configfs, efivarfs, devpts and similar, detected with `fstatfs`) are never read: reading them can block (`/proc/kmsg`) or have side effects. The walk still lists them, with the hash state "pseudo file system".
- The walk is depth first with the names of each directory in byte order, one open descriptor per level (the soft limit of open files is raised to the hard limit for this process). Link targets longer than 4096 bytes are refused, so a batch always fits in a frame.
- Patterns: sensitive paths and excludes are patterns (`crates/parity-collect/src/pattern.rs`). A pattern is a path whose components may contain `*`, matching any run of bytes within one component; it covers the paths that match it and everything below them.
- Sensitive paths: the built-in list is in `crates/parity-collect/src/sensitive.rs`. A hard link elsewhere to a sensitive file is sensitive too: collect mode notes device and inode of all sensitive regular files with more than one link (scanning at most 20,000 paths) and checks entries with more than one link against them.
- Collect mode needs these system calls, all present in Linux 3.2: `openat`, `newfstatat`, `fstat`, `fstatfs`, `readlinkat`, `getdents64`, `poll`, `fcntl`, `getrlimit`/`setrlimit`, `setresgid`/`setresuid`, `fork`, `setsid`, `dup2`.

## Comparison

- Packages: read from the dpkg database, not by running dpkg on the target (collect mode runs no programs, and `dpkg-query` would change access times). The CLI requests `/var/lib/dpkg/status`, then walks `/var/lib/dpkg/updates` and reads its journal entries (files named only with digits, in name order); each record there replaces the package's record from the status database, as dpkg and `dpkg-query` apply pending entries after an interrupted run. Packages are keyed by name and architecture and reported as only on one side, or with a different version or `Status` field (want, error flag and state, so a package on hold on one side only is a difference). Packages in state `not-installed` are ignored. `parity packages` shows the status abbreviated as `dpkg -l` and `dpkg-query -f '${db:Status-Abbrev}'` do.
- Files: both targets get the same roots, sorted and without nested roots, so both entry streams arrive in the same order and are merged like sorted lists. The CLI does not hold either tree in memory.
- Compared per path: type, mode (including setuid, setgid, sticky), UID, GID, size (regular files), link target, device number, content hash (XXH3-128, with the `xxhash-rust` crate). Modification times are not compared by default, because they differ between any two installations; an option turns them on. Directory sizes are never compared.
- Content hash: XXH3-128, which is not a cryptographic hash (decided 2026-11-26). Parity compares the same path on two machines, and it trusts root on both anyway: collect mode runs as root there, and whoever can change `/etc` or `/usr` can also change what collect mode reports. For finding changes, the chance that two different files at one path get the same 128-bit hash by accident is negligible (about 2⁻¹²⁸ per path). XXH3 is much faster than SHA-256 and needs no further crates on the root side. It does not resist deliberately crafted collisions: someone who can write a file on one machine could shape it to hash like the file on the other, and Parity would report both as identical. Parity is therefore no integrity checker. Because a hash that is not cryptographic may give away a short input, hashes of sensitive paths are never sent (see Decisions). The CLI shows the first 6 bytes of both hashes for a file whose content differs.
- A directory present on one side only is reported once, with the number of entries below it.
- Content diffs: only for regular files whose hashes differ, which are not sensitive and not larger than the diff size limit on either side. Contents are fetched in groups of 64 files from both targets in parallel. Text (valid UTF-8 without NUL bytes) gets a unified diff, anything else is reported as binary.
- Default etc directories of the configuration comparison (`--check etc`), can be replaced by `--etc-dirs`.
- Hashing on large file systems: every regular file up to the hash size limit (default 64 MiB) is hashed in one pass while walking; larger files are compared by metadata and listed as skipped. A smarter strategy (hash only where sizes match, or in a second pass) is possible later without protocol changes beyond a flag.
- Errors for single paths (for example an I/O error while hashing) are listed and make the exit code 2, since the comparison is then incomplete. Vanished files are ignored.
- Output: a report grouped by directory for people, JSON for scripts (`--format json`, paths converted lossily to UTF-8).

## Command line

Fixed for the first release (decided 2026-11-24). Renaming or removing a command or option breaks the scripts that use it.

- Commands: `parity compare [OPTIONS] <a> <b>`, `parity packages [OPTIONS] <target>`, `parity ping [OPTIONS] <target>`, `parity completion <SHELL>`. `collect` and `hold` are internal and do not appear in the help.
- On `compare`, `packages` and `ping`: `--become sudo|su|none` (default `sudo`), `--parity-path PATH` (default `parity`), `--ssh-config FILE` and `--ssh-option OPTION` (repeatable), passed on to ssh as `-F` and `-o`. `compare` and `packages` also take `--format text|json`.
- `compare` only: `--check` (comma separated `packages`, `etc`, `filesystem`; default `packages,etc`), `--etc-dirs PATHS`, `--exclude PATTERN` (repeatable), `--no-default-excludes`, `--sensitive PATTERN` (repeatable), `--reveal-sensitive-metadata`, `--max-hash-size SIZE`, `--max-diff-size SIZE`, `--no-diff`, `--with-mtime`.
- No short options. They can be added later without breaking anything, but once released they can never be removed.
- `--etc-dirs` belongs to `--check etc` and is named after it. It takes one comma separated list and may be given only once, so a path containing a comma cannot be given. Without `etc` in `--check` it is an error, not silently ignored.
- The opt-in that reports metadata of sensitive paths says in its name that it discloses something (`--reveal-sensitive-metadata`), because a size can give away the length of a secret. It never includes the hash.
- `--parity-path` follows rsync's `--rsync-path`. If the binary is ever renamed, the option is renamed with it.
- `--with-mtime`, not `--compare-mtime`, so the option does not repeat the command.
- `completion` works like `kubectl completion`: the shell is an argument, `bash`, `fish` or `zsh`, and the script goes to stdout, for `source <(parity completion bash)` or `parity completion fish | source`. Its help says how to load it. More shells can be added later without breaking anything.
- The completion scripts are rendered at run time from the clap definition with `clap_complete`, so they always match the help texts. `collect` and `hold` are not part of it. clap_complete's fish script completes no positional arguments, so fish offers no shells after `parity completion`. There are no man pages: `parity <command> --help` is the reference.

## Other documents

- [collect-mode.md](collect-mode.md): the exact behaviour of collect mode, for reviewers
- [security-review.md](security-review.md): the review of collect mode
- [maintaining.md](maintaining.md): toolchain, builds, releases, the Debian package and the settings on GitHub
- [tests/e2e/README.md](../tests/e2e/README.md): what the e2e tests cover
- [compare-with-base-install.md](compare-with-base-install.md): comparing a system with a fresh install of the same release

## Open points

- How the binary gets onto the target: installed beforehand (current assumption) or copied for one run (which would write to the target).
- Further architectures (see Decisions), and how the right binary for a target is chosen once there is more than one.
- Package state collection beyond Debian.
