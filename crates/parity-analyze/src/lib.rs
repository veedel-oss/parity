//! CLI side: SSH, privilege escalation, comparison and reports. Runs without
//! special privileges on the workstation.
//!
//! The command line is described in `docs/architecture.md` (Command line).
//! Renaming or removing a command or option breaks scripts that use it.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};
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
    /// Compare two targets
    Compare(CompareArgs),
    /// List the installed Debian packages of one target
    Packages {
        #[command(flatten)]
        conn: ConnArgs,
        /// Output format
        #[arg(long, value_enum, default_value = "text")]
        format: Format,
        /// ssh destination: host, user@host or ssh://[user@]host[:port]
        target: String,
    },
    /// Check that collect mode starts on a target and answers
    Ping {
        #[command(flatten)]
        conn: ConnArgs,
        /// ssh destination: host, user@host or ssh://[user@]host[:port]
        target: String,
    },
    /// Print the completion script for a shell
    #[command(after_help = COMPLETION_HELP)]
    Completion {
        /// Shell to complete for
        #[arg(value_enum)]
        shell: Shell,
    },
}

const COMPLETION_HELP: &str = "\
Load the completion in the current shell:

  bash:  source <(parity completion bash)
  fish:  parity completion fish | source
  zsh:   source <(parity completion zsh)

Add that line to ~/.bashrc, ~/.config/fish/config.fish or ~/.zshrc to load it \
in every new shell. The Debian package installs the completion for all three.";

/// The shells that `parity completion` prints a script for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Fish,
    Zsh,
}

#[derive(Args)]
struct ConnArgs {
    /// How to become root on the targets
    #[arg(long = "become", value_enum, default_value = "sudo")]
    become_: BecomeArg,
    /// Path of parity on the targets, or a command name looked up in the SSH user's PATH
    #[arg(long, value_name = "PATH", default_value = "parity")]
    parity_path: String,
    /// ssh configuration file (passed to ssh as -F)
    #[arg(long, value_name = "FILE")]
    ssh_config: Option<PathBuf>,
    /// ssh option (passed to ssh as -o), may be repeated
    #[arg(long = "ssh-option", value_name = "OPTION")]
    ssh_options: Vec<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum BecomeArg {
    /// sudo, asking for the SSH user's password if sudo needs one
    Sudo,
    /// su, asking for root's password
    Su,
    /// No escalation, the SSH user is root
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    /// Report for people
    Text,
    /// JSON for scripts
    Json,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Check {
    /// Installed Debian packages
    Packages,
    /// Configuration directories: /etc and /usr/local/etc, or --etc-dirs
    Etc,
    /// The whole file system, except the default excludes (see --no-default-excludes)
    Filesystem,
}

#[derive(Args)]
struct CompareArgs {
    #[command(flatten)]
    conn: ConnArgs,
    /// What to compare, comma separated
    #[arg(long, value_enum, value_delimiter = ',', default_values = ["packages", "etc"])]
    check: Vec<Check>,
    /// Directories for --check etc instead of /etc and /usr/local/etc, comma separated
    #[arg(long = "etc-dirs", value_name = "PATHS", value_delimiter = ',', action = ArgAction::Set)]
    etc_dirs: Vec<String>,
    /// Path to leave out, with everything below it (`*` matches within a name), may be repeated
    #[arg(long = "exclude", value_name = "PATTERN")]
    excludes: Vec<String>,
    /// Do not apply the default excludes of --check filesystem
    #[arg(long)]
    no_default_excludes: bool,
    /// Further sensitive path (`*` matches within a name), may be repeated
    #[arg(long = "sensitive", value_name = "PATTERN")]
    sensitive: Vec<String>,
    /// Also report size, modification time, link target and hash of sensitive paths (never their content)
    #[arg(long)]
    reveal_sensitive_metadata: bool,
    /// Files larger than this are not hashed (suffixes K, M, G)
    #[arg(long, value_name = "SIZE", default_value = "64M", value_parser = parse_size)]
    max_hash_size: u64,
    /// Files larger than this get no content diff (suffixes K, M, G)
    #[arg(long, value_name = "SIZE", default_value = "1M", value_parser = parse_size)]
    max_diff_size: u64,
    /// Do not fetch file contents for diffs
    #[arg(long)]
    no_diff: bool,
    /// Also report differing modification times
    #[arg(long)]
    with_mtime: bool,
    /// Output format
    #[arg(long, value_enum, default_value = "text")]
    format: Format,
    /// First target, an ssh destination: host, user@host or ssh://[user@]host[:port]
    target_a: String,
    /// Second target, an ssh destination
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
            parity_path: self.parity_path.clone(),
            ssh_config: self.ssh_config.clone(),
            ssh_options: self.ssh_options.clone(),
            ssh_program: "ssh".into(),
        }
    }
}

// The completion script for `shell`, as `parity completion` prints it and
// the Debian package installs it (crates/xtask). Rendered from the clap
// definition of the CLI, so it always matches the help texts. `collect` and
// `hold` are not part of it: the binary hands them over to collect mode
// before the CLI parses anything (crates/parity/src/main.rs).
pub fn completion(shell: Shell) -> Vec<u8> {
    let generator = match shell {
        Shell::Bash => clap_complete::Shell::Bash,
        Shell::Fish => clap_complete::Shell::Fish,
        Shell::Zsh => clap_complete::Shell::Zsh,
    };
    // Into a buffer: clap_complete panics if writing fails, for example
    // into a closed pipe.
    let mut script = Vec::new();
    clap_complete::generate(generator, &mut Cli::command(), "parity", &mut script);
    script
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
        Cmd::Completion { shell } => {
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(&completion(shell))
                .and_then(|()| stdout.flush())
                .map_err(|e| Error(format!("cannot write the completion script: {e}")))?;
            Ok(0)
        }
    }
}

fn file_options(a: &CompareArgs) -> Result<Option<FileOptions>> {
    let etc = a.check.contains(&Check::Etc);
    let fs = a.check.contains(&Check::Filesystem);
    if !a.etc_dirs.is_empty() && !etc {
        return Err(Error("--etc-dirs needs etc in --check".into()));
    }
    if !fs && !etc {
        return Ok(None);
    }
    let mut roots: Vec<Vec<u8>> = a
        .etc_dirs
        .iter()
        .map(|r| files::check_path(r))
        .collect::<Result<_>>()?;
    if fs {
        roots.push(b"/".to_vec());
    } else if roots.is_empty() {
        roots = DEFAULT_ETC_DIRS
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
            metadata: a.reveal_sensitive_metadata,
        },
        compare_mtime: a.with_mtime,
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

        let Cmd::Compare(a) = parse(&["compare", "--etc-dirs", "etc", "a", "b"])
            .unwrap()
            .cmd
        else {
            panic!()
        };
        assert!(file_options(&a).is_err());
    }

    #[test]
    fn collect_and_hold_are_not_commands() {
        let cmd = Cli::command();
        let names: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
        assert_eq!(names, ["compare", "packages", "ping", "completion"]);
    }

    #[test]
    fn completion_takes_one_of_three_shells() {
        for (arg, shell) in [
            ("bash", Shell::Bash),
            ("fish", Shell::Fish),
            ("zsh", Shell::Zsh),
        ] {
            let Cmd::Completion { shell: parsed } = parse(&["completion", arg]).unwrap().cmd else {
                panic!()
            };
            assert_eq!(parsed, shell);
        }
        for args in [
            &["completion"][..],
            &["completion", "powershell"],
            &["completion", "bash", "zsh"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn completions_have_every_command_and_option() {
        let cmd = Cli::command();
        let mut checked = 0;
        for shell in [Shell::Bash, Shell::Fish, Shell::Zsh] {
            let script = String::from_utf8(completion(shell)).unwrap();
            for sub in cmd.get_subcommands() {
                assert!(
                    script.contains(sub.get_name()),
                    "{} missing for {shell:?}",
                    sub.get_name()
                );
                // Without those clap adds (--help, --version).
                for long in sub.get_arguments().filter_map(|a| a.get_long()) {
                    let option = if shell == Shell::Fish {
                        format!("-l {long}")
                    } else {
                        format!("--{long}")
                    };
                    assert!(script.contains(&option), "{option} missing for {shell:?}");
                    checked += 1;
                }
            }
            // The shells after `parity completion`. clap_complete's fish
            // script completes no positional arguments.
            if shell != Shell::Fish {
                assert!(script.contains("bash fish zsh"), "{shell:?}");
            }
        }
        // --become, --parity-path, --ssh-config and --ssh-option on three
        // commands, --format on two and 10 options of compare only, for each
        // of the three shells.
        assert_eq!(checked, 3 * (4 * 3 + 2 + 10));
    }

    #[test]
    fn etc_dirs() {
        let Cmd::Compare(a) = parse(&["compare", "--etc-dirs", "/opt/app/etc,/etc", "a", "b"])
            .unwrap()
            .cmd
        else {
            panic!()
        };
        let o = file_options(&a).unwrap().unwrap();
        assert_eq!(o.roots, vec![b"/etc".to_vec(), b"/opt/app/etc".to_vec()]);

        // One comma separated list, the option is given once.
        assert!(
            parse(&[
                "compare",
                "--etc-dirs",
                "/etc",
                "--etc-dirs",
                "/srv",
                "a",
                "b"
            ])
            .is_err()
        );

        // Without etc in --check the directories would not be compared.
        let Cmd::Compare(a) = parse(&[
            "compare",
            "--check",
            "packages,filesystem",
            "--etc-dirs",
            "/srv",
            "a",
            "b",
        ])
        .unwrap()
        .cmd
        else {
            panic!()
        };
        assert!(file_options(&a).unwrap_err().0.contains("--etc-dirs"));
    }
}
