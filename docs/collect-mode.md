# Collect mode

The exact behaviour of collect mode, for reviewers. How it fits together is in [architecture.md](architecture.md), what was reviewed in [security-review.md](security-review.md), and the messages in [protocol.md](protocol.md).

## Requests and limits

Every path from the CLI must be absolute and normalised: no empty, `.` or `..` components, no trailing slash, no NUL byte (`crates/parity-collect/src/path.rs`).

| Limit | At most |
| --- | --- |
| a path | 4096 bytes |
| a request frame | 1 MiB |
| roots of a walk | 64 |
| excludes of a walk | 4096 |
| extra sensitive patterns | 256 |
| entries in a batch | 1000 |
| a link target | 4096 bytes, so a batch always fits in a frame |
| a file sent with `ReadFile` | 64 MiB |
| the dpkg status database | 256 MiB |
| paths scanned for hard-linked sensitive files | 20,000 |

## Opening files

Paths are opened step by step from `/` with `openat` and `O_NOFOLLOW`, so a symlink anywhere in a path is never followed. Directories that are only passed through are opened with `O_PATH` (Linux 2.6.39), which never changes an access time and, unlike `O_NOATIME`, does not need the caller to own the file. Directories and files that are read are opened with `O_NOATIME`. There is no fallback without it: a file that cannot be opened that way is reported as an error, rather than read with a changed access time.

Files are opened with `O_NONBLOCK` and checked with `fstat` after opening, so a FIFO swapped in after the `stat` cannot block the walk.

Files on pseudo file systems are never read, since reading them can block (`/proc/kmsg`) or have side effects: proc, sysfs, debugfs, tracefs, securityfs, cgroup, bpf, pstore, configfs, efivarfs, devpts and similar, detected with `fstatfs`. The walk still lists them, with the hash state "pseudo file system".

## Walk

The walk is depth first, with the names of each directory in byte order and one open descriptor per level. For this the soft limit of open files is raised to the hard limit, for collect mode only.

## Patterns and sensitive paths

Excludes and sensitive paths are patterns (`crates/parity-collect/src/pattern.rs`): a path whose components may contain `*`, matching any run of bytes within one component. A pattern covers the paths that match it and everything below them.

The built-in list of sensitive paths is in `crates/parity-collect/src/sensitive.rs`. A hard link elsewhere to a sensitive file is sensitive too: collect mode notes device and inode of every sensitive regular file with more than one link, and checks every entry with more than one link against them.

## System calls

Collect mode makes these system calls itself, all present in Linux 3.2:

`openat`, `open`, `newfstatat`, `fstat`, `fstatfs`, `readlinkat`, `getdents64`, `poll`, `fcntl`, `ioctl` (from `isatty`), `getrlimit`/`setrlimit`, `getuid`/`geteuid`, `setresgid`/`setresuid`, `rt_sigaction` (from `signal`), `fork`, `setsid`, `dup2`, `close`, `exit_group` (from `_exit`).

## Minimum kernel

The Rust documentation states no separate minimum for the musl targets (musl 1.2.5 is linked in). The baseline of the Rust standard library is the one stated for the gnu target: Linux 3.2 for x86_64. Collect mode uses only system calls that exist there (see [System calls](#system-calls)).

## Attaching

With sudo or su, collect mode takes over the pipes of `parity hold` (see [architecture.md](architecture.md#two-sessions-over-one-connection)). The holder prints one line, `PARITY-HOLD <protocol> <version> <pid> <path of the binary in hex>`. It then keeps stdin, stdout and stderr open without reading or writing them: it polls them with no requested events, which reports only a hangup.

Before collect mode takes over the holder's pipes, it checks (`crates/parity-collect/src/attach.rs`):

| Check | Why |
| --- | --- |
| All UIDs of the holder equal the user who escalated: `SUDO_UID`, or with su the owner of the terminal, which sshd sets to the login user | only that user's own session can be taken over |
| The holder's GIDs are uniform | as for the UIDs |
| The holder's command line is exactly `<path> hold <nonce>` | the nonce ties the two sessions together |
| `/proc/<pid>/fd/0`, `1` and `2` are opened with the holder's real and effective UID and GID, the saved IDs stay 0 | the kernel's same-user rule applies, so no `CAP_SYS_PTRACE` is needed (containers and hardened systems drop it); before Linux 4.5 the kernel checks the real IDs here, from 4.5 on the file system IDs |
| After switching back to root: the descriptors are pipes, and the holder is still the same process | nothing was swapped in between |

If switching back to root fails, collect mode exits. Otherwise it ignores SIGHUP, forks and detaches (`setsid`) with the pipes as stdin, stdout and stderr. The parent exits, so sudo or su return.

## The binary run as root

The holder refuses to start if a third user could replace the binary that is about to run as root (`crates/parity-collect/src/hold.rs`). The binary and every directory above it must belong to root or the SSH user, and must not be writable by others, except directories with the sticky bit. Group write is accepted: Debian ships `/usr/local/bin` as `root:staff` 2775, and sudo's `secure_path` trusts it the same way.

## End of a session

Collect mode exits on the end of its input and on a failed write. During a walk it also polls stdout for a hangup after every entry and every 16 MiB while hashing, and ends the walk.
