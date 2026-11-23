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

# use su instead of sudo on the targets
parity compare --become su host-a host-b
```

Targets are ssh destinations, so host aliases, users and ports from your `~/.ssh/config` apply.

The command and option names are placeholders. The final options will follow with the first release.

## Roadmap

- Support for more distributions
- Comparison against a saved snapshot instead of two live machines
- Export of differences as Ansible or Terraform code

## Contributing

Issues and pull requests are welcome. Please open an issue first for larger changes.

## License

Parity is licensed under the GNU Affero General Public License v3.0 or later. See the [LICENSE](LICENSE) file for the full text.

Parity is part of [veedel-oss](https://github.com/veedel-oss).