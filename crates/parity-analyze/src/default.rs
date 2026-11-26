//! Defaults of the comparison:
//!  - The directories of `--check etc`
//!  - The excluded path of `--check filesystem`

// Directories of --check etc, unless --etc-dirs replaces them.
pub const DEFAULT_ETC_DIRS: &[&str] = &["/etc", "/usr/local/etc"];

// Excluded by default when comparing the whole file system: virtual and
// runtime file systems, temporary files, caches and logs.
pub const DEFAULT_FS_EXCLUDES: &[&str] = &[
    //// Runtime and ephemeral
    // device files, dynamically created
    "/dev",
    // kernel process info (virtual FS, never compare)
    "/proc",
    // kernel/sysfs, also virtual
    "/sys",
    // runtime state, PID files, sockets
    "/run",
    "/var/run",
    // Lock file for processes
    "/var/lock",
    // boot files, kernel images, initrd images
    "/boot",
    "/initrd.img",
    "/initrd.img.old",
    "/vmlinuz",
    "/vmlinuz.old",
    // Swap files and swap images
    "/swap",
    "/swap.img",
    //// tmp, logs, cache, spools
    // Temporary files (deleted between system reboots on modern systems)
    "/tmp",
    // Temporary files persist across reboots (unlike /tmp)
    "/var/tmp",
    // Stores system and application log files
    "/var/log",
    // Holds cached data that programs can regenerate if deleted
    "/var/cache",
    // Temporary files of applications
    "/var/lib/*/tmp",
    // queued jobs (mail, print, cron outputs)
    "/var/spool",
    // systemd runtime state
    "/var/lib/systemd",
    // common mount path
    "/media",
    "/mnt",
    // dynamic linker cache
    "/etc/ld.so.cache",
    // prelinking cache for libraries.
    "/etc/prelink.cache",
    // fsck data fragments
    "/lost+found",
    // apt metadata
    "/var/lib/apt/lists",
];
