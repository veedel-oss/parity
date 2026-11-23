# Comparing a system with a fresh install of its release

Use this to find out what was added or changed on a legacy or manually set up machine, for example before describing it as infrastructure as code. Command and option names are working names until the first release.

## 1. Set up the base

Install a fresh machine (a VM is enough) with the same release and architecture as the system to inspect, for example Debian 12 on amd64. Use the same installation method where you know it (netinst, cloud image, debootstrap), because each brings a slightly different package set. Do not configure anything on it apart from SSH access and sudo for your user.

Install the same version of `parity` on both machines, for example in `/usr/local/bin`.

## 2. Compare

Put the base first, so "only on <system>" lists what was added:

```bash
parity compare base-vm legacy-host
```

By default this compares installed packages and the configuration directories `/etc` and `/usr/local/etc`, including a diff for every text file whose content differs. To include the whole file system:

```bash
parity compare --check packages,filesystem base-vm legacy-host
```

The file system comparison leaves out `/proc`, `/sys`, `/dev`, `/run`, temporary files, caches and logs. Add more excludes for paths that always differ and do not interest you:

```bash
parity compare --check filesystem \
  --exclude /etc/hostname --exclude /etc/machine-id --exclude /var/lib/dhcp \
  base-vm legacy-host
```

For scripts, `--format json` prints the same result as JSON. The exit code is 0 if the machines are identical, 1 if they differ and 2 if the comparison could not be completed.

## 3. Read the result

- **Packages**: packages only on the system were installed on top of the base. A different version usually means the system is not up to date, or the base is newer; update both or compare against a base from the same point release.
- **Files only on the system**: added configuration, local scripts, data. A directory that exists on one side only is shown once, with the number of entries below it.
- **Changed files**: type, mode, owner, group, size, link target and content are compared. Text files get a unified diff. Modification times are not compared unless you add `--compare-mtime`.
- **Skipped on purpose**: sensitive paths such as `/etc/shadow` or SSH host keys are compared by type, mode, owner and group only. Their content never leaves the machine. Files above the hash size limit (`--max-hash-size`, default 64M) are compared by metadata only.
- Owners and groups are compared as numeric UIDs and GIDs. System users created in a different order get different numbers and show up as differences.

Expected differences between any two installations: SSH host keys (sensitive, skipped), `/etc/hostname`, `/etc/hosts`, `/etc/machine-id`, network configuration, `/var/lib/dpkg` and `/var/lib/apt` state files.

## Tested

`tests/vm/run.sh` runs this workflow: for each tested Debian release it compares a fresh VM with a second one from the same image in which a configuration file was changed, a mode and an owner were changed and a directory tree was added, and checks that exactly these show up. It also checks that two fresh installs compare as identical (exit code 0).
