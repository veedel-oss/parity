# Changelog

## 0.0.1 (2026-11-24)

First release.

- `parity compare <a> <b>` compares two Linux machines over SSH: installed Debian packages, configuration directories (`/etc` and `/usr/local/etc`, or `--etc-dirs`) including unified diffs of changed text files, and with `--check filesystem` the whole file system.
- Reports as a table grouped by directory, or as JSON with `--format json`. Exit codes: 0 identical, 1 different, 2 error.
- Packages are read from the dpkg database including pending journal entries (`/var/lib/dpkg/updates`), as `dpkg-query` sees them; version, architecture and the full status (for example a package on hold) are compared.
- `parity packages <target>` lists installed Debian packages with their status as `dpkg -l` abbreviates it, `parity ping <target>` checks that collect mode starts.
- `parity completion bash|fish|zsh` prints the shell completion script, for example for `source <(parity completion bash)` in `~/.bashrc`.
- Privilege escalation on the targets with sudo (default), su or none (`--become`). The password is typed on your terminal and goes to sudo or su directly; Parity never sees it.
- Read-only on the targets: no files written, access times unchanged, symlinks never followed, pseudo file systems never read.
- Sensitive paths such as `/etc/shadow` and SSH host keys are compared by type, mode, owner and group only; their content is never sent. Add paths with `--sensitive`.
- `--exclude` and `--sensitive` take path patterns: `*` matches within one name, as in `/var/lib/*/tmp`, and a pattern covers everything below the paths it matches.
- One static binary for x86_64 Linux, built against musl. Supported and tested targets: Debian 10, 11, 12 and 13 (Linux 4.19 to 6.12).
- A Debian package installs that binary as `/usr/bin/parity`.
