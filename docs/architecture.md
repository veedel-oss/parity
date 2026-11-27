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

- One binary for both roles. The CLI starts collect mode over SSH for one run. Reasons: root privileges exist only for one run, nothing needs to be set up or configured on the targets apart from installing the binary, and authorisation comes from the target's existing sudo or su setup.
- Collect and analysis code stay in separate crates (see Components), so the part that runs as root stays small and reviewable.
- Collect mode refuses to run unless it runs as root (effective UID 0), with a clear error message. The CLI role needs no privileges.
- Only users who can become root on a target (through sudo or su) can inspect it with Parity. Whoever can do that has read access to the whole file system anyway, Parity adds no new access.
- Parity itself leaves no trace on the inspected system. Collect mode opens every file and directory with `O_NOATIME`, so reading does not change access times (with plain reads, `relatime` would still update them about once a day; `stat` never touches them). It writes no files: no state, no cache, no temporary files. Logs go to stderr and reach the CLI. sudo and su log the escalation as usual, that is the target's own audit trail and intended.
- Sensitive paths (for example `/etc/shadow`, `/etc/gshadow`, `/etc/ssh/ssh_host_*_key`, `/etc/ssl/private`, `/root/.ssh`, `/home/*/.ssh`) never leak information about their content by default. Collect mode reports only existence, type, mode, owner and group for them. Size, modification time, link target, device number, hash and content are withheld, because a size can reveal the length of a secret and a hash can give away a short one. The list is built in. The user running the CLI (who is root-capable on the target anyway) can add paths with an explicit option and can opt in to the metadata of sensitive paths with an explicit option. The hash and the content of sensitive files are never transferred: the content hash is not cryptographic (see Comparison). The default protects reports and terminals from secrets, it is not a security boundary against the root-capable user.
- Collect mode serves file contents on request, for the content comparison of configuration files. The CLI fetches contents only for files whose hashes differ, within a size limit.
- Parity runs on every Linux system that can be reached over SSH, old and new (supported and tested: Debian 10 to Debian 13, on the target as well as on the workstation). Therefore it is written in Rust and statically linked against musl: it needs no libc or other libraries on the target, and the same binary runs everywhere. A target needs the `parity` binary, an SSH server and sudo or su. The remaining limits are the minimum kernel of the Rust target (currently 3.2), one build per CPU architecture, and package collection, which needs support per distribution.
- Collect mode uses newer system calls only with a fallback, so it keeps working on old kernels (for example `statx` from 4.11, `openat2` from 5.6; Debian 10 ships 4.19). Not following symlinks is done with `openat` and `O_NOFOLLOW` step by step, not with `openat2`.
- Collect mode reports owner and group as raw UID and GID and does no name lookups (no NSS, no libc user database).
- License: AGPL-3.0-or-later.
- Name and distribution: the binary and the project stay `parity`. The crates are not published on crates.io (`publish = false`): `parity` is taken there by an unrelated crate, the `parity-` prefix is in practice that of Parity Technologies (`parity-scale-codec`, `parity-wasm`, `parity-db`), and a target needs the static musl binary, which `cargo install` would not build. Releases are on GitHub: the static binary and a Debian package made from it (see Builds). Checked 2026-11-23: Repology lists no distribution package named `parity`, only `parity-git` in the AUR (the discontinued Parity Ethereum client, whose binary was also called `parity`).
- Architectures: x86_64 only, the one the e2e tests run on. A further architecture is built, tested and released together, once CI can run the e2e tests on it.
- No rsync. An rsync-based comparison depends too much on the network setup (sudo on both targets, SSH port forwarding between them for itemized changes).
- Privilege escalation shares one SSH connection through two multiplexed sessions, a holder and an escalation session, with collect mode attaching to the holder's pipes (see Connection model). Alternatives that were ruled out: a terminal for the whole session (the protocol would pass through a line discipline, and the password would have to be relayed by the CLI); `sudo -S` (the password would pass through Parity); `sudo -v` in a first session (the sudo timestamp is bound to the terminal, a second session does not share it, and su has no timestamp at all); a socket or FIFO on the target (would write to the target and open a local channel to a root process).
- The protocol is a compact binary format written for Parity, not JSON lines, so `parity-collect` needs no serialisation library (see Protocol).
- Collect mode depends only on `parity-proto`, `libc` and `xxhash-rust` (XXH3-128 for content hashes, see Comparison; hashes and security-relevant algorithms always come from well-known libraries, never from this repository). The CLI takes the nonces for attaching from `rand` (a ChaCha CSPRNG seeded from the system's random source).
- The CLI uses clap without its default features `color` and `suggestions` (decided 2026-11-26): help and errors are plain text, without "did you mean" hints, which keeps the seven crates for terminal colours and string similarity out of the binary.
- `parity compare` exit codes follow `diff` and `cmp`: 0 targets identical, 1 differences found, 2 error (the comparison could not be completed, for example a target unreachable, `parity` not found on the target, escalation failed, a protocol error). Files skipped on purpose (size limit, sensitive paths, excluded paths) are listed in the report but do not change the exit code.

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
- `completion` (added 2026-11-25) works like `kubectl completion`: the shell is an argument, `bash`, `fish` or `zsh`, and the script goes to stdout, for `source <(parity completion bash)` or `parity completion fish | source`. Its help says how to load it. More shells can be added later without breaking anything.

## Builds

- Toolchain: `rust-toolchain.toml` pins one Rust version, with rustfmt, clippy and the musl target, for every build: CI, the e2e tests, releases and local development. The release binary is therefore built with the toolchain the e2e tests ran with, and the toolchain changes only with a commit; Dependabot proposes updates after a 7-day wait. This is the supported Rust version.
- Static release build for `x86_64-unknown-linux-musl`: no C code is compiled, and `rust-lld` from the pinned toolchain links it against the musl that comes with Rust's target, not the system's linker (`.cargo/config.toml`). The config also states `-C target-feature=+crt-static`, although static linking is musl's default today, because Rust plans to make dynamic linking the default (rust-lang/compiler-team#422), and `-C link-self-contained=yes`, so the musl of the pinned toolchain is used, never one installed on the build machine. A `RUSTFLAGS` from the environment would replace these flags without an error, so CI denies warnings with `CARGO_BUILD_WARNINGS=deny` (Cargo's `build.warnings`) instead of `RUSTFLAGS=-D warnings`. Everything built for the build machine itself (development builds and tests, and the build scripts and proc macros of the dependencies) is linked with its `cc`, so building needs a C compiler. CI builds the binary and checks static linking and a size limit of 5 MB.
- Repository and organisation settings, set by the maintainer on GitHub, not in the repository, so that only the maintainer can publish a release: the maintainer is the only organisation owner, the base permission gives members no write access, and nobody else has write, maintain or admin access to the repository; two-factor authentication is required. Rulesets let only organisation owners push `v*` tags and make the CI jobs required status checks (so renaming a job needs a change of the ruleset). Actions allows only actions created by GitHub and requires full-length commit SHAs, the default `GITHUB_TOKEN` is read-only and cannot create or approve pull requests, and workflows from fork pull requests of external contributors need approval. Releases are immutable. Dependabot alerts and security updates, Secret Protection (secret scanning and push protection) and private vulnerability reporting are enabled. The workflows fit these settings: only `actions/*`, each pinned to a full commit SHA, and write access only in the release job.
- Releases: pushing a signed tag (`git tag -s v<version> -m "Release v<version>"`) runs `.github/workflows/release.yml`, in two jobs. The build job builds the x86_64 binary, checks that it is static and that `parity --version` matches the tag, and builds the Debian package from it. It runs the build scripts of the dependencies, so it has read access only, no OIDC token and no stored credentials. The release job starts only after the maintainer's approval in the environment `release` and runs no code of the project: it attests the build provenance of the binary and the package (`actions/attest`, checked with `gh attestation verify <file> --repo veedel-oss/parity`) and creates a draft release with the binary (`parity-<version>-x86_64-unknown-linux-musl`), the package (`parity_<version>_amd64.deb`), `SHA256SUMS` of both and the section of `CHANGELOG.md` for that version. The maintainer publishes the draft. Until then the section of the version in `CHANGELOG.md` is headed `## <version> (unreleased)`, without a date; the tagged commit drops the marker, and the build job stops if the heading is not exactly `## <version>`. The workflow does not run the tests: tag a commit on `main` only after `ci.yml` and `e2e.yml` passed on it.
- Debian package: built in the release only, by `packaging/deb/build.sh BINARY OUTDIR` with `dpkg-deb`, from the binary that was built and checked, as it is. CI and the e2e tests test the binary, and the release checks that the binary in the package is byte-identical to it, so the package holds the tested bytes. The script builds it locally the same way; `make build-deb` runs it in a container built from `packaging/deb/Dockerfile`, for hosts without dpkg-deb, whose Debian base image is pinned by digest and updated by Dependabot like the one of the e2e tests. dpkg-deb comes with dpkg on every Debian and Ubuntu system, so the release installs no packaging tool and builds none from source; until 2026-11-25 it used cargo-deb, which needed a pinned version built in the release job, a manifest only for Dependabot to update that pin, and a build step in `crates/xtask`, all now removed. The package installs `/usr/bin/parity` (a package must not install into `/usr/local`; `/usr/bin` passes the holder's ownership checks and is on the SSH user's `PATH` and in sudo's `secure_path`, so `--parity-path` keeps its default), the changelog (`changelog.gz`), the `copyright` file and nothing else, with an `md5sums` file for `dpkg --verify`. It has no completions (`parity completion` prints them) and no man pages. It has no maintainer scripts, conffiles or services, so installing configures nothing and removing leaves nothing behind, and no dependencies or recommends: the binary is static, and the workstation needs an ssh client while a target needs an SSH server and sudo or su, which one package for both roles cannot express. Its version is the one the binary reports, without a Debian revision, since releases are immutable. It is compressed with xz, because dpkg before 1.21.18 (Debian 10 and 11) cannot unpack zstd. Every file time is `SOURCE_DATE_EPOCH`, by default the time of the commit, so the same commit gives the same package (checked with dpkg 1.22.6 and 1.22.22, also on a single CPU).
- Shell completions: `parity completion` renders them at run time from the CLI's clap definition with `clap_complete`, so they always match the help texts. `collect` and `hold` are not part of the clap definition and are not completed. clap_complete's fish script completes commands, options and their values, but no positional arguments, so fish offers no shells after `parity completion`. The Debian package does not install them, users load them with `parity completion` (decided 2026-11-25). There are no man pages (decided 2026-11-25): `parity <command> --help` is the reference.
- License notices: `packaging/deb/copyright` (Debian's machine-readable format) names everything linked into the static binary with its copyright holders and license: Parity (AGPL-3.0-or-later with the full text, which `/usr/share/common-licenses` does not have), every crate from crates.io, the Rust standard library, LLVM's libunwind and musl. CI checks that it lists exactly the crates of `cargo tree -p parity --target x86_64-unknown-linux-musl -e normal,no-proc-macro`.
- Minimum kernel: the Rust documentation states no separate minimum for the musl targets (musl 1.2.5 is linked in). The Rust standard library's baseline is the one stated for the gnu target: Linux 3.2 for x86_64. Collect mode uses only system calls available there.
- The e2e tests (`tests/e2e/run.sh OS`, needs KVM, also in CI on GitHub's runners as one job per OS) run on virtual machines with their own kernels. The OS is one of `debian-10`, `debian-11`, `debian-12`, `debian-13`; nothing else is tested. Each run boots two VMs from the latest genericcloud image of that release from cloud.debian.org (the `latest` folder, no checksum check; cached and downloaded again when the server has a newer one) with the binary in `/usr/local/bin`, over real ssh with sudo, su and as root. They cover escalation and connection errors, the checks of the installed binary, the two fresh installs comparing as identical and changes to one showing up, dpkg's journal, access times on ext4, no process left behind after errors and Ctrl-C, and the workspace tests as root. Older kernels down to 3.2 are kept working by using only their system calls, not by tests. There are no container tests: containers share the host kernel, so VMs cover everything they would. qemu itself runs in a container built from `tests/e2e/Dockerfile`, whose Debian base image is pinned by digest; Dependabot proposes a new digest when the tag moves, after a 7-day wait.

## Open points

- How the binary gets onto the target: installed beforehand (current assumption) or copied for one run (which would write to the target).
- Further architectures (see Decisions), and how the right binary for a target is chosen once there is more than one.
- Package state collection beyond Debian.
