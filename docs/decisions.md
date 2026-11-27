# Decisions

The decisions behind Parity, each with its reason. How Parity works is in [architecture.md](architecture.md).

## Roles and privileges

- **One binary for both roles.** The CLI starts collect mode over SSH for one run: root privileges exist only for that run, a target needs nothing set up apart from the binary, and authorisation comes from its existing sudo or su setup.
- **Collecting and analysing in separate crates** (see [architecture.md](architecture.md#components)), so the code that runs as root stays small and reviewable.
- **Collect mode runs only as root** (effective UID 0) and says so otherwise. The CLI needs no privileges.
- **Only users who can become root on a target can inspect it.** They can read the whole file system anyway, so Parity adds no access.
- **Escalation over two sessions of one SSH connection** (see [architecture.md](architecture.md#connection-model)). Ruled out:

  | Alternative | Why not |
  | --- | --- |
  | a terminal for the whole session | the protocol would pass through a line discipline, and the CLI would have to relay the password |
  | `sudo -S` | the password would pass through Parity |
  | `sudo -v` in a first session | the sudo timestamp is bound to the terminal, a second session does not share it, and su has no timestamp at all |
  | a socket or FIFO on the target | it would write to the target and open a local channel to a root process |

## Reading the target

- **No trace on the target.** Collect mode writes no files, no state, cache or temporary files, and leaves access times unchanged, which with plain reads `relatime` would still update about once a day (see [collect-mode.md](collect-mode.md#opening-files)). Its logs go to stderr and reach the CLI. sudo and su log the escalation as usual: that is the target's own audit trail.
- **Sensitive paths reveal nothing about their content by default.** For paths such as `/etc/shadow`, `/etc/ssh/ssh_host_*_key` or `/home/*/.ssh`, collect mode reports only existence, type, mode, owner and group. Size, modification time, link target, device number, hash and content are withheld: a size can reveal the length of a secret, and a hash can give away a short one, as the content hash is not cryptographic (see [architecture.md](architecture.md#comparison)).
- **The user can add sensitive paths and opt in to more metadata,** but the hash and content of a sensitive file are never sent. The built-in list keeps secrets out of reports and terminals; it is no boundary against the root-capable user.
- **File contents only on request,** for the diff of configuration files: the CLI fetches only files whose hashes differ, within a size limit.
- **UIDs and GIDs as numbers.** Collect mode does no name lookups: no NSS, no libc user database.

## Where it runs

- **Rust, statically linked against musl,** so that the same binary runs on every Linux system, old and new, and needs no libraries on the target. Supported and tested are Debian 10 to 13, as target and as workstation. A target needs the binary, an SSH server and sudo or su. What stays limited: the minimum kernel of the Rust target (Linux 3.2), one build per CPU architecture, and package collection, which needs support for each distribution.
- **Newer system calls only with a fallback,** so old kernels keep working: `statx` is from Linux 4.11, `openat2` from 5.6, and Debian 10 ships 4.19. Symlinks are not followed thanks to `openat` with `O_NOFOLLOW` step by step, not `openat2`.
- **x86_64 only,** the architecture the e2e tests run on. Another one is built, tested and released together, once CI can run the e2e tests on it.

## Protocol and dependencies

- **A compact binary protocol of its own,** not JSON lines, so collect mode needs no serialisation library (see [architecture.md](architecture.md#protocol)).
- **Collect mode depends only on `parity-proto`, `libc` and `xxhash-rust`** (XXH3-128 for content hashes, see [architecture.md](architecture.md#content-hash)). Hashes and other security-relevant algorithms come from well-known libraries, never from this repository. The CLI takes the nonces for attaching from `rand`, a ChaCha CSPRNG seeded from the system's random source.
- **clap without its features `color` and `suggestions`.** Help and errors are plain text without "did you mean" hints, which keeps the seven crates for terminal colours and string similarity out of the binary.

## Output, name and distribution

- **Exit codes as with `diff` and `cmp`:** 0 identical, 1 differences found, 2 error, when the comparison could not be completed (a target unreachable, `parity` not found on it, escalation failed, a protocol error). Files skipped on purpose (size limit, sensitive or excluded paths) are listed in the report but do not change the exit code.
- **No rsync.** A comparison with rsync depends too much on the network setup: sudo on both targets, and SSH port forwarding between them for itemized changes.
- **The name stays `parity`, and the crates are not published on crates.io** (`publish = false`). There `parity` is taken by an unrelated crate, the `parity-` prefix is in practice that of Parity Technologies (`parity-scale-codec`, `parity-wasm`, `parity-db`), and `cargo install` would not build the static musl binary a target needs. Releases are on GitHub, with the static binary and a Debian package made from it (see [maintaining.md](maintaining.md#releases)).
- **The Debian package is called `parity` too.** No distribution has a package of that name: Repology lists only `parity-git` in the AUR, the discontinued Parity Ethereum client, whose binary was also called `parity`.
- **License:** AGPL-3.0-or-later.
