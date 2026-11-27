# Parity: Architecture

How Parity works, for anyone who wants to understand or change it. Why it works that way is in [decisions.md](decisions.md). The exact behaviour of collect mode, the protocol and how Parity is built and released are in the documents listed under [Other documents](#other-documents).

## Purpose

Parity compares two Linux machines, their installed packages, configuration files and file system, and reports where they differ. Typical uses are finding configuration drift in a cluster, and comparing a legacy or manually set up system with a fresh install of the same release, for example Debian 12, to bring existing machines under infrastructure as code.

It runs on every Linux system that can be reached over SSH. Package collection supports Debian first.

## Core design principle

Parity is a single binary with two roles:

```
workstation                           target a
parity compare a b  ─── ssh ───────▶  parity collect, as root, read-only
no privileges,      ◀── raw data ────
compares, reports                     target b
                    ─── ssh ───────▶  parity collect, as root, read-only
                    ◀── raw data ────
```

On the workstation it is the CLI: it needs no special privileges, connects to the targets over SSH, compares and reports. On each target the CLI starts the same binary in collect mode, for one run. Collect mode runs as root, is strictly read-only, and only collects raw data: package states, paths, metadata, hashes, and file contents where requested. All analysis lives in the CLI. This keeps the code that runs as root small, and lets the analysis grow without touching the collector.

## Components

The roles are subcommands of the one binary:

| Command | Run by | Runs on |
| --- | --- | --- |
| `parity compare`, `packages`, `ping`, `completion` | the user | the workstation, as the CLI |
| `parity collect` | the CLI, over SSH | a target, as root |
| `parity hold <nonce>` | the CLI, over SSH, with sudo or su (see Connection model) | a target, as the SSH user |

`collect` and `hold` are not meant to be run by hand. The commands and options are under Command line.

The binary is built from separate crates, libraries rather than programs:

| Crate | What it does | Runs |
| --- | --- | --- |
| `parity` | the binary: hands `collect` and `hold` to `parity-collect` before any other argument parsing, so no CLI code runs as root | on both sides |
| `parity-collect` | collect mode: reads requests from stdin, answers on stdout, logs to stderr and exits; read-only, no network, no SSH, no analysis | on a target, as root |
| `parity-analyze` | the CLI: starts SSH, handles escalation, speaks the protocol, compares and reports | on the workstation |
| `parity-proto` | the protocol of both sides: messages, version, framing, errors | on both sides |

`parity-collect` and `parity-analyze` never depend on each other; both depend only on `parity-proto` for what they share. `parity-collect` keeps its dependency list short, because it runs as root: every dependency there needs a reason.

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

## Protocol

The CLI and collect mode speak a compact binary protocol of their own, over the stdin and stdout of the SSH session. Each side first sends a hello with its protocol version, then the CLI sends requests and collect mode answers each in order. It is implemented in `parity-proto`, without dependencies. Framing, encoding and the requests are in [protocol.md](protocol.md), and how collect mode handles them in [collect-mode.md](collect-mode.md).

## Comparison

The CLI compares what collect mode sends from both targets. `--check` chooses what: packages, configuration directories or the whole file system.

### Packages

Packages come from the dpkg database, not from running dpkg on the target: collect mode runs no programs, and `dpkg-query` would change access times. The CLI reads `/var/lib/dpkg/status`, then the pending journal entries in `/var/lib/dpkg/updates`, the files named only with digits, in name order. Each entry replaces the record of its package, as dpkg and `dpkg-query` apply them after an interrupted run.

Packages are matched by name and architecture. A package differs if it is on one side only, or has another version or `Status` field. The `Status` field holds want, error flag and state, so a package on hold on one side only is a difference. Packages in state `not-installed` are left out. `parity packages` abbreviates the status as `dpkg -l` does.

### Files

`--check etc` walks `/etc` and `/usr/local/etc` (`DEFAULT_ETC_DIRS` in [default.rs](../crates/parity-analyze/src/default.rs)), unless `--etc-dirs` names others. `--check filesystem` walks `/` without the default excludes (`DEFAULT_FS_EXCLUDES`).

Both targets get the same roots, sorted and without nested roots, so the two entry streams arrive in the same order and are merged like sorted lists. The CLI holds neither tree in memory.

Compared for each path:

| What | When |
| --- | --- |
| type, mode with setuid, setgid and sticky, UID, GID | always |
| link target, device number | for symlinks and devices |
| size | for regular files; directory sizes never |
| content hash | for regular files up to `--max-hash-size`, hashed while walking; larger ones are compared by metadata and listed as skipped |
| modification time | only with `--with-mtime`, since it differs between any two installations |

A directory present on one side only is reported once, with the number of entries below it.

### Content hash

The content hash is XXH3-128 (`xxhash-rust`), which is not a cryptographic hash. Parity compares the same path on two machines whose root it trusts anyway: whoever can change `/etc` or `/usr` can also change what collect mode reports. The chance that two different files at one path get the same 128-bit hash by accident is about 2⁻¹²⁸. XXH3 is much faster than SHA-256 and needs no further crates on the root side.

It does not resist deliberately crafted collisions: someone who can write a file on one machine could shape it to hash like the file on the other, and Parity would report both as identical. Parity is therefore no integrity checker. Hashes of sensitive paths are never sent (see [decisions.md](decisions.md#reading-the-target)). For a file whose content differs, the report shows the first 6 bytes of both hashes.

### Content diffs

Only regular files get a content diff: those whose hashes differ, that are not sensitive and not larger than `--max-diff-size` on either side. The CLI fetches their contents from both targets in parallel, in groups of 64 files. Text, that is valid UTF-8 without NUL bytes, gets a unified diff; anything else is reported as binary.

### Errors and output

An error for a single path, for example while hashing, is listed and makes the exit code 2, since the comparison is then incomplete. Files that vanish during the walk are ignored.

The report for people is grouped by directory. `--format json` is for scripts; it converts paths to UTF-8, lossily.

## Command line

The commands and options are fixed for the first release: renaming or removing one breaks the scripts that use it. They are the ones `parity --help` and `parity <command> --help` show, and the [README](../README.md#usage) lists them with examples. `collect` and `hold` are internal and not in the help.

### Names

| Choice | Why |
| --- | --- |
| no short options | they can be added later without breaking anything, but once released never removed |
| `--parity-path` | follows rsync's `--rsync-path`; if the binary is ever renamed, the option is renamed with it |
| `--with-mtime`, not `--compare-mtime` | the option does not repeat the command |
| `--reveal-sensitive-metadata` | says in its name that it discloses something, since a size can give away the length of a secret; it never includes the hash |
| `--etc-dirs` | belongs to `--check etc` and is named after it |

`--etc-dirs` takes one comma separated list and may be given only once, so a path containing a comma cannot be given. Without `etc` in `--check` it is an error, not silently ignored.

### Completions

`parity completion <shell>` works like `kubectl completion`: the shell, `bash`, `fish` or `zsh`, is an argument, and the script goes to stdout, for `source <(parity completion bash)` or `parity completion fish | source`. Its help says how to load it. More shells can be added later without breaking anything.

The scripts are rendered at run time from the clap definition with `clap_complete`, so they always match the help texts. `collect` and `hold` are not part of them. clap_complete's fish script completes no positional arguments, so fish offers no shells after `parity completion`.

There are no man pages: `parity <command> --help` is the reference.

## Open points

- How the binary gets onto the target: installed beforehand (current assumption) or copied for one run (which would write to the target).
- Further architectures (see [decisions.md](decisions.md#where-it-runs)), and how the right binary for a target is chosen once there is more than one.
- Package state collection beyond Debian.
- Hashing on large file systems: hashing only where the sizes match, or in a second pass. It needs no protocol change beyond a flag.

## Other documents

- [decisions.md](decisions.md): the decisions behind Parity, each with its reason
- [collect-mode.md](collect-mode.md): the exact behaviour of collect mode, for reviewers
- [protocol.md](protocol.md): framing, encoding and the requests of the protocol
- [security-review.md](security-review.md): the review of collect mode
- [maintaining.md](maintaining.md): toolchain, builds, releases, the Debian package and the settings on GitHub
- [tests/e2e/README.md](../tests/e2e/README.md): what the e2e tests cover
- [compare-with-base-install.md](compare-with-base-install.md): comparing a system with a fresh install of the same release
