# Parity

<img alt="Parity logo" width="128" src="./assets/logo.svg">

Parity compares two Linux machines and shows where they differ, from files and config to packages. A single static binary, read-only on the systems it inspects. Ob jleich oder nit, Parity weiß et.

## Why Parity?

Clusters drift. Two nodes that should be identical slowly diverge: a changed config file here, a different package version there. Parity finds these differences quickly, so you can see at a glance whether two systems are really the same.

It also helps with infrastructure as code for existing systems: compare a legacy or manually set up machine with a fresh install of the same release (for example Debian 11) and see exactly what was added or changed on top.

## Features

- Compares installed package versions
- Compares configuration directories such as `/etc`, including the content of the files
- Compares the file systems of two machines (paths, permissions, ownership, size and checksums)
- Gives a fast overview of whether two systems differ, and where
- Read-only: Parity reports differences and never changes a system
- Runs on any Linux you can reach over SSH, old or new: one statically linked binary written in Rust, no libraries or runtimes needed on the target systems

## Status

Parity is in early development and not usable yet. In the first release, the package comparison will support Debian-based systems only.

## How it works

Parity is one binary with two roles. On your workstation it is the CLI. For each machine you want to compare, the CLI connects over SSH and starts the same binary there in collect mode, with sudo (or su, if you prefer). Collect mode runs as root to read the whole file system, but it only ever reads. It answers the CLI's requests and exits when the run is over. The CLI compares the results locally.

Each target only needs the `parity` binary, a running SSH server and sudo or su. No SSH port forwarding is needed.

**Design principle:** collect mode stays minimal. It only collects raw data and sends it back, read-only. All analysis, from the comparison itself to evaluating the results, lives in the CLI, which needs no special privileges.

**Connection:** the CLI uses your system's `ssh` client and your existing SSH setup (keys, agent, password, ssh config). Encryption and host verification come from SSH. If sudo or su asks for a password, you type it into your terminal; it never passes through Parity.

**Access:** only users who can become root on a target can inspect it with Parity. Parity adds no access of its own. Secrets such as `/etc/shadow` or SSH host keys are left out of reports by default.

## Usage

```bash
# on your workstation; parity is installed on both targets
parity compare host-a ssh://user@host-b:2222
```

Targets are ssh destinations (`host`, `user@host` or `ssh://[user@]host[:port]`), so host aliases, users, ports and jump hosts from your `~/.ssh/config` apply.

By default, `parity compare` compares the installed packages and the configuration directories `/etc` and `/usr/local/etc`, with a diff for every text file whose content differs. `--check` selects what to compare:

```bash
# packages and the whole file system (without /proc, /sys, /dev, /run, temporary files, caches and logs)
parity compare --check packages,filesystem host-a host-b

# other configuration directories
parity compare --check etc --etc-dirs /etc,/opt/app/etc host-a host-b

# leave out paths that always differ
parity compare --exclude /etc/hostname --exclude /etc/machine-id host-a host-b

# su instead of sudo on the targets
parity compare --become su host-a host-b
```

| Option | Meaning |
| --- | --- |
| `--check LIST` | What to compare, comma separated: `packages`, `etc`, `filesystem`. Default: `packages,etc` |
| `--etc-dirs PATHS` | Directories for `etc` instead of `/etc` and `/usr/local/etc`, comma separated |
| `--exclude PATH` | Leave out a path and everything below it, may be repeated |
| `--no-default-excludes` | Also compare `/proc`, `/tmp`, caches, logs and the other default excludes of `filesystem` |
| `--sensitive PATTERN` | Treat more paths as sensitive (`*` matches within a name), may be repeated |
| `--reveal-sensitive-metadata` | Also report size, modification time, link target and hash of sensitive paths. Their content is never sent |
| `--max-hash-size SIZE` | Larger files are compared by metadata only. Default: `64M` |
| `--max-diff-size SIZE` | Larger files get no content diff. Default: `1M` |
| `--no-diff` | No content diffs |
| `--with-mtime` | Also report differing modification times |
| `--format json` | JSON for scripts instead of the report |
| `--become METHOD` | How to become root on the targets: `sudo` (default), `su`, or `none` if you log in as root |
| `--parity-path PATH` | Path of `parity` on the targets. Default: `parity` from the SSH user's `PATH` |
| `--ssh-config FILE`, `--ssh-option OPTION` | Passed on to ssh as `-F` and `-o` |

Exit codes of `parity compare`, as with `diff`: 0 identical, 1 differences found, 2 error (the comparison could not be completed).

`parity ping <target>` checks that Parity starts on a target and answers. `parity packages <target>` lists the installed packages of one target. Both take the `--become`, `--parity-path` and ssh options. `parity <command> --help` shows all options.

To find out what was added or changed on a system compared with a fresh install of its release, see [Comparing a system with a fresh install](docs/compare-with-base-install.md).

## Roadmap

- Support for more distributions
- Comparison against a saved snapshot instead of two live machines
- Export of differences as Ansible or Terraform code

## Contributing

Issues and pull requests are welcome. Please open an issue first for larger changes.

## License

Parity is licensed under the GNU Affero General Public License v3.0 or later. See the [LICENSE](LICENSE) file for the full text.

Parity is part of [veedel-oss](https://github.com/veedel-oss).