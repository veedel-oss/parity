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
- `parity hold <nonce>`: unprivileged helper on the target for escalated sessions (see Connection model). Not meant to be run by hand.

The CLI also has `parity packages <target>` (list installed packages) and `parity ping <target>` (check that collect mode starts). All command and option names are working names until the first release.

Internally the binary is built from separate crates (Rust libraries, not separate programs) with clear boundaries:

- `parity`: the binary crate. Decides the role and hands over. If the first argument is `collect` or `hold`, it calls into `parity-collect` before any other argument parsing, so no CLI code runs as root.
- `parity-collect`: library for collect mode. Runs on the target as root, reads requests from stdin, answers on stdout, writes logs to stderr and exits. Read-only, no network, no SSH, no analysis.
- `parity-analyze`: library for the CLI side. Starts SSH, handles privilege escalation, speaks the protocol, compares and produces reports.
- `parity-proto`: library for the protocol shared by both sides: request and response types, version, framing, errors.

Boundary rules: `parity-collect` and `parity-analyze` never depend on each other; both depend only on `parity-proto` for what they share. `parity-collect` keeps its dependency list short, because it runs as root. Every dependency there needs a reason.

## Connection model

The CLI uses the system's `ssh` binary, so the user's keys, agent, password, ssh config and jump hosts apply. Authentication, encryption and host verification come from SSH. Collect mode handles one session: it reads requests until the end of input or an explicit end request, answers each, and exits. It also exits when writing to stdout fails, so it never outlives the SSH session. No SSH port forwarding is needed. This fails only if the SSH user is restricted to fixed commands (for example by ForceCommand).

Privilege escalation (working name of the option `--become`):

- `sudo` is the default.
- `su` with a password prompt, for systems without sudo (su is part of every Debian installation).
- `none` for when the SSH user is already root.

The password never passes through Parity. Parity has no password option, never reads it, never stores it and never relays it. The prompt from sudo or su goes to the user's terminal and the user types the password there. When two targets need a password, the CLI handles the prompts one after another, then collects from both in parallel.

How the prompt and the protocol share one SSH connection (decided after prototyping against Debian 9 and 13 with sudo and su, see "Decisions"):

- With `--become none` there is one session, `ssh -T -- <target> parity collect`, and the protocol runs over its stdin and stdout.
- With sudo or su the CLI opens two sessions over one SSH connection, using OpenSSH connection multiplexing with a private control socket on the workstation (`ssh -M -S <socket> -o ControlPersist=no`, the second session with `-S <socket>`). No port forwarding is involved.
  1. Protocol session, without a terminal: `parity hold <nonce>` runs as the SSH user. It prints one line (`PARITY-HOLD <protocol> <version> <pid> <path of the binary in hex>`) and then keeps the session's stdin, stdout and stderr open without reading or writing them (it polls them with no requested events, which reports only a hangup). The CLI checks the protocol version here, before anyone is asked for a password.
  2. Escalation session, with a terminal (`ssh -tt`), whose stdin, stdout and stderr are the CLI's own terminal: `sudo -- <path> collect --attach <pid> <nonce>`, or `su -c '<path> collect --attach <pid> <nonce>' root`. sudo or su prompts on that terminal and the user types into ssh directly; the CLI process never sees a byte of it.
  3. Collect mode, now root, checks the holder: all its UIDs equal the user who escalated (`SUDO_UID`, or for su the owner of the terminal, which sshd sets to the login user), its GIDs are uniform, and its command line is exactly `<path> hold <nonce>`. It then opens `/proc/<pid>/fd/0`, `1` and `2` with the holder's real and effective UID and GID, keeping 0 as saved IDs (so the kernel's same-user rule applies and no `CAP_SYS_PTRACE` is needed; containers and hardened systems drop it; before Linux 4.5 the kernel checks the real IDs for this, from 4.5 on the file system IDs, so switching only the effective IDs fails on old kernels), switches back to root, checks that the descriptors are pipes and that the holder is still the same, ignores SIGHUP, forks and detaches (`setsid`) with those pipes as stdin, stdout and stderr. The parent exits, so sudo or su return and the escalation session ends.
  4. The CLI speaks the protocol over the protocol session.
- The holder refuses to start if a third user could replace the binary that is about to run as root: the binary and every directory above it must belong to root or the SSH user and must not be writable by others (sticky directories excepted). Group write is accepted, because Debian ships `/usr/local/bin` as `root:staff` 2775 and sudo's `secure_path` trusts it the same way.
- When the session ends or the CLI is interrupted, ssh closes the pipes: collect mode sees end of input or a write error and exits (during a walk it also polls stdout for a hangup after every entry and every 16 MiB while hashing, and ends the walk), the holder sees the hangup and exits. Nothing stays behind, the VM tests check this after Ctrl-C in the middle of hashing a 16 GB file.

Remote path: the CLI runs `parity` from the SSH user's `PATH` by default (the holder is started that way) and has an option for a different remote path (working name `--remote-path`). The escalated command uses the absolute path the holder reports, so sudo's `secure_path` and su's environment do not matter and the same binary runs in both sessions. A missing binary is therefore always detected in the unprivileged session (exit code 126 or 127), and the CLI points to `--remote-path`.

Version check: the first message of every session carries the protocol version, in an encoding that never changes. With escalation the holder line carries it too, so a mismatch is reported before the password prompt. If the versions of the CLI and the remote binary do not match, the CLI stops with a clear error naming both versions.

Error messages: ssh failures (exit code 255), binary not found (126, 127), collect mode not running as root with `--become none`, escalation refused or wrong password (the escalation session ends with a non-zero status; sudo's or su's own message is already on the terminal), version mismatch, and output that is not the protocol (for example from shell startup files) each get their own message.

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
- Parity runs on every Linux system that can be reached over SSH, old and new (supported and tested: Debian 10 to Debian 13, on the target as well as on the workstation). Therefore it is written in Rust and statically linked against musl: it needs no libc or other libraries on the target, and the same binary runs everywhere. A target needs the `parity` binary, an SSH server and sudo or su. The remaining limits are the minimum kernel of the Rust target (currently 3.2), one build per CPU architecture, and package collection, which needs support per distribution.
- Collect mode uses newer system calls only with a fallback, so it keeps working on old kernels (for example `statx` from 4.11, `openat2` from 5.6; Debian 10 ships 4.19). Not following symlinks is done with `openat` and `O_NOFOLLOW` step by step, not with `openat2`.
- Collect mode reports owner and group as raw UID and GID and does no name lookups (no NSS, no libc user database). Where names are needed, the CLI maps the numbers itself, using `/etc/passwd` and `/etc/group` from the collected data.
- License: AGPL-3.0-or-later.
- No rsync. An rsync-based comparison depends too much on the network setup (sudo on both targets, SSH port forwarding between them for itemized changes).
- Privilege escalation shares one SSH connection through two multiplexed sessions, a holder and an escalation session, with collect mode attaching to the holder's pipes (see Connection model). Alternatives that were ruled out: a terminal for the whole session (the protocol would pass through a line discipline, and the password would have to be relayed by the CLI); `sudo -S` (the password would pass through Parity); `sudo -v` in a first session (the sudo timestamp is bound to the terminal, a second session does not share it, and su has no timestamp at all); a socket or FIFO on the target (would write to the target and open a local channel to a root process).
- The protocol is a compact binary format written for Parity, not JSON lines, so `parity-collect` needs no serialisation library (see Protocol).
- Collect mode depends only on `parity-proto`, `libc` and `sha2` (RustCrypto, for SHA-256; security-relevant algorithms always come from well-known libraries, never from this repository). The CLI takes the nonces for attaching from `rand` (a ChaCha CSPRNG seeded from the system's random source).
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
- Sensitive paths: the built-in list is in `crates/parity-collect/src/sensitive.rs`. Patterns may contain `*` within one component; everything below a matching path is sensitive. A hard link elsewhere to a sensitive file is sensitive too: collect mode notes device and inode of all sensitive regular files with more than one link (scanning at most 20,000 paths) and checks entries with more than one link against them.
- Collect mode needs these system calls, all present in Linux 3.2: `openat`, `newfstatat`, `fstat`, `fstatfs`, `readlinkat`, `getdents64`, `poll`, `fcntl`, `getrlimit`/`setrlimit`, `setresgid`/`setresuid`, `fork`, `setsid`, `dup2`.

## Comparison

- Packages: read from the dpkg database, not by running dpkg on the target (collect mode runs no programs, and `dpkg-query` would change access times). The CLI requests `/var/lib/dpkg/status`, then walks `/var/lib/dpkg/updates` and reads its journal entries (files named only with digits, in name order); each record there replaces the package's record from the status database, as dpkg and `dpkg-query` apply pending entries after an interrupted run. Packages are keyed by name and architecture and reported as only on one side, or with a different version or `Status` field (want, error flag and state, so a package on hold on one side only is a difference). Packages in state `not-installed` are ignored. `parity packages` shows the status abbreviated as `dpkg -l` and `dpkg-query -f '${db:Status-Abbrev}'` do.
- Files: both targets get the same roots, sorted and without nested roots, so both entry streams arrive in the same order and are merged like sorted lists. The CLI does not hold either tree in memory.
- Compared per path: type, mode (including setuid, setgid, sticky), UID, GID, size (regular files), link target, device number, content hash (SHA-256, with the `sha2` crate). Modification times are not compared by default, because they differ between any two installations; an option turns them on. Directory sizes are never compared.
- A directory present on one side only is reported once, with the number of entries below it.
- Content diffs: only for regular files whose hashes differ, which are not sensitive and not larger than the diff size limit on either side. Contents are fetched in groups of 64 files from both targets in parallel. Text (valid UTF-8 without NUL bytes) gets a unified diff, anything else is reported as binary.
- Default roots of the configuration comparison: `/etc` and `/usr/local/etc`. The file system comparison walks `/` with default excludes for virtual and runtime file systems, temporary files, caches and logs (`/dev`, `/lost+found`, `/media`, `/mnt`, `/proc`, `/run`, `/sys`, `/tmp`, `/var/cache`, `/var/lib/apt/lists`, `/var/log`, `/var/tmp`); an option turns them off.
- Hashing on large file systems: every regular file up to the hash size limit (default 64 MiB) is hashed in one pass while walking; larger files are compared by metadata and listed as skipped. A smarter strategy (hash only where sizes match, or in a second pass) is possible later without protocol changes beyond a flag.
- Errors for single paths (for example an I/O error while hashing) are listed and make the exit code 2, since the comparison is then incomplete. Vanished files are ignored.
- Output: a report grouped by directory for people, JSON for scripts (`--format json`, paths converted lossily to UTF-8).

## Builds

- Static release builds for `x86_64-unknown-linux-musl` (about 1.2 MB) and `aarch64-unknown-linux-musl` (about 1.0 MB), both linked with `rust-lld`, no C toolchain needed. CI checks static linking and a size limit of 5 MB.
- Minimum kernel: the Rust documentation states no separate minimum for the musl targets (musl 1.2.5 is linked in). The Rust standard library's baseline is the one stated for the gnu targets: Linux 3.2 for x86_64, Linux 4.1 for aarch64. Collect mode uses only system calls available on these.
- System tests (`tests/vm/run.sh OS`, needs KVM, also in CI on GitHub's runners as one job per OS) run on virtual machines with their own kernels. The OS is one of `debian-10`, `debian-11`, `debian-12`, `debian-13`; nothing else is tested. Each run boots two VMs from the latest genericcloud image of that release from cloud.debian.org (the `latest` folder, no checksum check; cached and downloaded again when the server has a newer one) with the binary in `/usr/local/bin`, over real ssh with sudo, su and as root. They cover escalation and connection errors, the checks of the installed binary, the two fresh installs comparing as identical and changes to one showing up, dpkg's journal, access times on ext4, no process left behind after errors and Ctrl-C, and the workspace tests as root. Older kernels down to 3.2 are kept working by using only their system calls, not by tests. There are no container tests: containers share the host kernel, so VMs cover everything they would.

## Open points

- How the binary gets onto the target: installed beforehand (current assumption) or copied for one run (which would write to the target).
- Which CPU architectures get a release build (x86_64 and aarch64 build today), and how the right binary for a target is chosen.
- Final command line options and option names.
- Package state collection beyond Debian.
- The binary name `parity` may clash with other tools of that name. Checked 2026-11-23: the crate name `parity` on crates.io is taken (an unrelated small crate), `parity-collect`, `parity-analyze` and `parity-proto` are free. Repology lists no distribution package named `parity`, only `parity-git` in the AUR (the discontinued Parity Ethereum client, whose binary was also called `parity`). Decide on a name or a crates.io name (for example publishing the binary crate under another name) before the first release.
