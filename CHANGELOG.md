# Changelog

## Unreleased (draft notes for 0.1.0)

First release. Command and option names may still change before it is tagged.

- `parity compare <a> <b>` compares two Linux machines over SSH: installed Debian packages, configuration directories (`/etc`, `/usr/local/etc` or `--root`) including unified diffs of changed text files, and with `--check filesystem` the whole file system.
- Reports as a table grouped by directory, or as JSON with `--format json`. Exit codes: 0 identical, 1 different, 2 error.
- Packages are read from the dpkg database including pending journal entries (`/var/lib/dpkg/updates`), as `dpkg-query` sees them; version, architecture and the full status (for example a package on hold) are compared.
- `parity packages <target>` lists installed Debian packages with their status as `dpkg -l` abbreviates it, `parity ping <target>` checks that collect mode starts.
- Privilege escalation on the targets with sudo (default), su or none (`--become`). The password is typed on your terminal and goes to sudo or su directly; Parity never sees it.
- Read-only on the targets: no files written, access times unchanged, symlinks never followed, pseudo file systems never read.
- Sensitive paths such as `/etc/shadow` and SSH host keys are compared by type, mode, owner and group only; their content is never sent. Add paths with `--sensitive`.
- One static binary for x86_64 and aarch64 Linux, built against musl. Supported and tested targets: Debian 10, 11, 12 and 13 (Linux 4.19 to 6.12).
