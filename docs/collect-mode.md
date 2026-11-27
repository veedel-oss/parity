# Collect mode

The exact behaviour of collect mode, for reviewers. How it fits together is in [architecture.md](architecture.md), what was reviewed in [security-review.md](security-review.md).

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

## Minimum kernel

The Rust documentation states no separate minimum for the musl targets (musl 1.2.5 is linked in). The baseline of the Rust standard library is the one stated for the gnu target: Linux 3.2 for x86_64. Collect mode uses only system calls that exist there, listed in [architecture.md](architecture.md#collect-mode-details).
