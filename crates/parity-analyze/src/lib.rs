//! CLI side: SSH, privilege escalation, comparison and reports. Runs without
//! special privileges on the workstation.
//!
//! Command and option names are working names until the first release.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use parity_proto::SensitivePolicy;

pub mod client;
pub mod error;
pub mod files;
pub mod packages;
pub mod report;
pub mod ssh;

use error::{Error, Result};
use files::FileOptions;
use report::{EXIT_ERROR, Report};
use ssh::{Become, Connection, SshOptions};

// Configuration directories compared by default.
pub const DEFAULT_CONFIG_ROOTS: &[&str] = &["/etc", "/usr/local/etc"];

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

#[derive(Parser)]
#[command(
    name = "parity",
    version,
    about = "Compare two Linux machines and show where they differ",
    after_help = "Exit codes of compare: 0 identical, 1 differences found, 2 error."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    // Compare two targets
    Compare(CompareArgs),
    // List the installed Debian packages of one target
    Packages {
        #[command(flatten)]
        conn: ConnArgs,
        #[arg(long, value_enum, default_value = "text")]
        format: Format,
        // ssh destination: host, user@host or ssh://[user@]host[:port]
        target: String,
    },
    // Check that collect mode starts on a target and answers
    Ping {
        #[command(flatten)]
        conn: ConnArgs,
        // ssh destination: host, user@host or ssh://[user@]host[:port]
        target: String,
    },
}

#[derive(Args)]
struct ConnArgs {
    // How to become root on the targets
    #[arg(long = "become", value_enum, default_value = "sudo")]
    become_: BecomeArg,
    // Path of parity on the targets
    #[arg(long, value_name = "PATH", default_value = "parity")]
    remote_path: String,
    // ssh configuration file (passed to ssh as -F)
    #[arg(long, value_name = "FILE")]
    ssh_config: Option<PathBuf>,
    // ssh option (passed to ssh as -o), may be repeated
    #[arg(long = "ssh-option", value_name = "OPTION")]
    ssh_options: Vec<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum BecomeArg {
    Sudo,
    Su,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Check {
    // Installed Debian packages
    Packages,
    // Configuration directories (/etc, /usr/local/etc or --root)
    Config,
    // The whole file system
    Filesystem,
}

#[derive(Args)]
struct CompareArgs {
    #[command(flatten)]
    conn: ConnArgs,
    // What to compare, comma separated
    #[arg(long, value_enum, value_delimiter = ',', default_values = ["packages", "config"])]
    check: Vec<Check>,
    // Configuration directory to compare instead of the defaults, may be repeated
    #[arg(long = "root", value_name = "PATH")]
    roots: Vec<String>,
    // Path to leave out, with everything below it, may be repeated
    #[arg(long = "exclude", value_name = "PATH")]
    excludes: Vec<String>,
    // Do not apply the default excludes of the file system comparison
    #[arg(long)]
    no_default_excludes: bool,
    // Further sensitive path (`*` matches within a name), may be repeated
    #[arg(long = "sensitive", value_name = "PATTERN")]
    sensitive: Vec<String>,
    // Also report size, time, link target and hash of sensitive paths (never their content)
    #[arg(long)]
    sensitive_metadata: bool,
    // Files larger than this are not hashed (suffixes K, M, G)
    #[arg(long, value_name = "SIZE", default_value = "64M", value_parser = parse_size)]
    max_hash_size: u64,
    // Files larger than this get no content diff (suffixes K, M, G)
    #[arg(long, value_name = "SIZE", default_value = "1M", value_parser = parse_size)]
    max_diff_size: u64,
    // Do not fetch contents for diffs
    #[arg(long)]
    no_diff: bool,
    // Also report differing modification times
    #[arg(long)]
    compare_mtime: bool,
    #[arg(long, value_enum, default_value = "text")]
    format: Format,
    // First target (ssh destination)
    target_a: String,
    // Second target (ssh destination)
    target_b: String,
}

pub fn parse_size(s: &str) -> std::result::Result<u64, String> {
    let (num, mul) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 1u64 << 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 1 << 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    num.parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(mul))
        .ok_or_else(|| format!("invalid size '{s}'"))
}

impl ConnArgs {
    fn options(&self) -> SshOptions {
        SshOptions {
            become_: match self.become_ {
                BecomeArg::Sudo => Become::Sudo,
                BecomeArg::Su => Become::Su,
                BecomeArg::None => Become::None,
            },
            remote_path: self.remote_path.clone(),
            ssh_config: self.ssh_config.clone(),
            ssh_options: self.ssh_options.clone(),
            ssh_program: "ssh".into(),
        }
    }
}

// Entry point of the CLI. Returns the exit code.
pub fn main(args: Vec<OsString>) -> i32 {
    let cli = match Cli::try_parse_from(std::iter::once(OsString::from("parity")).chain(args)) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_ERROR } else { 0 };
        }
    };
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("parity: {e}");
            EXIT_ERROR
        }
    }
}

fn run(cli: Cli) -> Result<i32> {
    match cli.cmd {
        Cmd::Ping { conn, target } => {
            let mut c = Connection::open(&target, &conn.options())?;
            c.ping()?;
            println!(
                "{target}: collect mode running, parity {} (protocol {})",
                c.remote.version, c.remote.protocol
            );
            c.close()?;
            Ok(0)
        }
        Cmd::Packages {
            conn,
            format,
            target,
        } => {
            let mut c = Connection::open(&target, &conn.options())?;
            let list = c.dpkg_packages()?;
            c.close()?;
            let Some(list) = list else {
                return Err(Error(format!(
                    "{target} has no dpkg database; package collection supports Debian-based systems only"
                )));
            };
            match format {
                Format::Text => print!("{}", report::packages_text(&list)),
                Format::Json => println!(
                    "{}",
                    serde_json::to_string_pretty(&list).expect("serialises")
                ),
            }
            Ok(0)
        }
        Cmd::Compare(a) => compare(a),
    }
}

fn file_options(a: &CompareArgs) -> Result<Option<FileOptions>> {
    let fs = a.check.contains(&Check::Filesystem);
    if !fs && !a.check.contains(&Check::Config) {
        return Ok(None);
    }
    let mut roots: Vec<Vec<u8>> = a
        .roots
        .iter()
        .map(|r| files::check_path(r))
        .collect::<Result<_>>()?;
    if fs {
        roots.push(b"/".to_vec());
    } else if roots.is_empty() {
        roots = DEFAULT_CONFIG_ROOTS
            .iter()
            .map(|r| r.as_bytes().to_vec())
            .collect();
    }
    let mut excludes: Vec<Vec<u8>> = a
        .excludes
        .iter()
        .map(|r| files::check_path(r))
        .collect::<Result<_>>()?;
    if fs && !a.no_default_excludes {
        excludes.extend(DEFAULT_FS_EXCLUDES.iter().map(|r| r.as_bytes().to_vec()));
    }
    excludes.sort();
    excludes.dedup();
    let extra = a
        .sensitive
        .iter()
        .map(|r| files::check_path(r))
        .collect::<Result<_>>()?;
    Ok(Some(FileOptions {
        roots: files::normalise_roots(roots),
        excludes,
        max_hash_size: a.max_hash_size,
        sensitive: SensitivePolicy {
            extra,
            metadata: a.sensitive_metadata,
        },
        compare_mtime: a.compare_mtime,
        content: !a.no_diff,
        max_content_size: a.max_diff_size,
    }))
}

fn compare(a: CompareArgs) -> Result<i32> {
    let file_opts = file_options(&a)?;
    let opts = a.conn.options();
    for t in [&a.target_a, &a.target_b] {
        ssh::validate_target(t)?;
    }
    // One after the other, so password prompts do not mix.
    let mut ca = Connection::open(&a.target_a, &opts)?;
    let mut cb = Connection::open(&a.target_b, &opts)?;
    let targets = [a.target_a.clone(), a.target_b.clone()];
    let mut notes = Vec::new();

    let packages = if a.check.contains(&Check::Packages) {
        let (sa, sb) = std::thread::scope(|s| {
            let h = s.spawn(|| ca.dpkg_packages());
            let sb = cb.dpkg_packages();
            (h.join().expect("thread panicked"), sb)
        });
        match (sa?, sb?) {
            (Some(x), Some(y)) => Some(packages::compare(&x, &y)),
            (x, y) => {
                let missing: Vec<&str> = [(x.is_none(), &targets[0]), (y.is_none(), &targets[1])]
                    .iter()
                    .filter(|(m, _)| *m)
                    .map(|(_, t)| t.as_str())
                    .collect();
                notes.push(format!(
                    "packages not compared: no dpkg database on {} (package collection supports Debian-based systems only)",
                    missing.join(" and ")
                ));
                None
            }
        }
    } else {
        None
    };

    let files = match &file_opts {
        Some(o) => Some(files::compare([&mut ca, &mut cb], o)?),
        None => None,
    };
    ca.close()?;
    cb.close()?;

    let report = Report::new(targets, packages, files, notes);
    match a.format {
        Format::Text => print!("{}", report.text()),
        Format::Json => print!("{}", report.json()),
    }
    Ok(report.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("10"), Ok(10));
        assert_eq!(parse_size("2K"), Ok(2048));
        assert_eq!(parse_size("64M"), Ok(64 << 20));
        assert_eq!(parse_size("1g"), Ok(1 << 30));
        assert!(parse_size("x").is_err());
        assert!(parse_size("99999999999G").is_err());
    }

    fn parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("parity").chain(args.iter().copied()))
    }

    #[test]
    fn option_like_targets_are_rejected() {
        // clap treats it as an unknown option ...
        assert!(parse(&["compare", "-oProxyCommand=x", "b"]).is_err());
        // ... and after `--` the target check rejects it.
        let Cmd::Compare(a) = parse(&["compare", "--", "-oProxyCommand=x", "b"])
            .unwrap()
            .cmd
        else {
            panic!()
        };
        assert!(
            compare(a)
                .unwrap_err()
                .0
                .contains("must not start with '-'")
        );
    }

    #[test]
    fn file_option_defaults() {
        let Cmd::Compare(a) = parse(&["compare", "a", "b"]).unwrap().cmd else {
            panic!()
        };
        let o = file_options(&a).unwrap().unwrap();
        assert_eq!(o.roots, vec![b"/etc".to_vec(), b"/usr/local/etc".to_vec()]);
        assert!(o.excludes.is_empty());

        let Cmd::Compare(a) = parse(&[
            "compare",
            "--check",
            "filesystem",
            "--exclude",
            "/srv",
            "a",
            "b",
        ])
        .unwrap()
        .cmd
        else {
            panic!()
        };
        let o = file_options(&a).unwrap().unwrap();
        assert_eq!(o.roots, vec![b"/".to_vec()]);
        assert!(o.excludes.contains(&b"/proc".to_vec()) && o.excludes.contains(&b"/srv".to_vec()));

        let Cmd::Compare(a) = parse(&["compare", "--check", "packages", "a", "b"])
            .unwrap()
            .cmd
        else {
            panic!()
        };
        assert!(file_options(&a).unwrap().is_none());

        let Cmd::Compare(a) = parse(&["compare", "--root", "etc", "a", "b"]).unwrap().cmd else {
            panic!()
        };
        assert!(file_options(&a).is_err());
    }
}
