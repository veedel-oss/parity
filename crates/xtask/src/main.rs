//! Release tool of Parity, never shipped and not linked into `parity`.
//!
//! `cargo run -p xtask -- gen-docs OUTDIR` writes the man pages and shell
//! completions of the Debian package, rendered from the CLI's clap
//! definition, so they always match the help texts:
//!
//! - `OUTDIR/man/parity.1` and one page per command (`parity-compare.1`, ...)
//! - `OUTDIR/completions/parity.bash`, `_parity` (zsh) and `parity.fish`
//!
//! The release workflow runs it before cargo-deb, which installs the files
//! (`[package.metadata.deb]` in `crates/parity/Cargo.toml`).

use std::path::Path;
use std::process::ExitCode;

use clap::Command;
use clap_complete::Shell;
use clap_mangen::Man;

const USAGE: &str = "usage: cargo run -p xtask -- gen-docs OUTDIR";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [task, out] if task == "gen-docs" => match gen_docs(Path::new(out)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("xtask: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

// Writes the man pages and completions below `out`.
fn gen_docs(out: &Path) -> std::io::Result<()> {
    let cmd = parity_analyze::command();
    for (dir, files) in [
        ("man", man_pages(cmd.clone())?),
        ("completions", completions(cmd)?),
    ] {
        let dir = out.join(dir);
        std::fs::create_dir_all(&dir)?;
        for (name, content) in files {
            std::fs::write(dir.join(name), content)?;
        }
    }
    Ok(())
}

// One page for `parity` and one for each command, as file name and roff
// source. clap's own `help` command gets none.
fn man_pages(cmd: Command) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    fn add(cmd: &Command, source: &str, pages: &mut Vec<(String, Vec<u8>)>) -> std::io::Result<()> {
        // Commands have no version of their own, so every page names that of parity.
        let man = Man::new(cmd.clone()).source(source);
        let mut roff = Vec::new();
        man.render(&mut roff)?;
        pages.push((man.get_filename(), roff));
        for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
            add(sub, source, pages)?;
        }
        Ok(())
    }

    let mut cmd = cmd.disable_help_subcommand(true);
    // Sets the display names of the commands, which name their pages (parity-compare).
    cmd.build();
    let source = format!("parity {}", cmd.get_version().unwrap_or_default());
    let mut pages = Vec::new();
    add(&cmd, &source, &mut pages)?;
    Ok(pages)
}

// The completions for bash, zsh and fish, as file name and script.
fn completions(mut cmd: Command) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    [
        (Shell::Bash, "parity.bash"),
        (Shell::Zsh, "_parity"),
        (Shell::Fish, "parity.fish"),
    ]
    .into_iter()
    .map(|(shell, name)| {
        let mut script = Vec::new();
        clap_complete::generate(shell, &mut cmd, "parity", &mut script);
        Ok((name.to_owned(), script))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(content: &[u8]) -> &str {
        std::str::from_utf8(content).unwrap()
    }

    // The long options of a command, without those clap adds (--help, --version).
    fn long_options(cmd: &Command) -> Vec<&str> {
        cmd.get_arguments()
            .filter(|a| !a.is_hide_set())
            .filter_map(|a| a.get_long())
            .collect()
    }

    #[test]
    fn a_page_for_parity_and_each_command() {
        let pages = man_pages(parity_analyze::command()).unwrap();
        let names: Vec<&str> = pages.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "parity.1",
                "parity-compare.1",
                "parity-packages.1",
                "parity-ping.1"
            ]
        );
        for (name, roff) in &pages {
            let roff = text(roff);
            assert!(roff.starts_with(".ie \\n(.g .ds Aq"), "{name}");
            assert!(
                roff.contains(&format!("\"parity {}\"", env!("CARGO_PKG_VERSION"))),
                "{name}"
            );
        }
        assert!(text(&pages[0].1).contains("Exit codes of compare"));
    }

    #[test]
    fn man_pages_have_every_option() {
        let cmd = parity_analyze::command();
        let pages = man_pages(cmd.clone()).unwrap();
        let mut checked = 0;
        for sub in cmd.get_subcommands() {
            let page = pages
                .iter()
                .find(|(n, _)| *n == format!("parity-{}.1", sub.get_name()))
                .map(|(_, roff)| text(roff))
                .unwrap();
            for long in long_options(sub) {
                // roff escapes the dashes.
                let option = format!("\\-\\-{}", long.replace('-', "\\-"));
                assert!(
                    page.contains(&option),
                    "{option} missing in parity-{}.1",
                    sub.get_name()
                );
                checked += 1;
            }
        }
        // --become, --parity-path, --ssh-config and --ssh-option on all three,
        // --format on two and 10 options of compare only.
        assert_eq!(checked, 4 * 3 + 2 + 10);
    }

    #[test]
    fn completions_have_every_command_and_option() {
        let cmd = parity_analyze::command();
        let scripts = completions(cmd.clone()).unwrap();
        assert_eq!(scripts.len(), 3);
        for (name, script) in &scripts {
            let script = text(script);
            for sub in cmd.get_subcommands() {
                assert!(
                    script.contains(sub.get_name()),
                    "{} missing in {name}",
                    sub.get_name()
                );
                for long in long_options(sub) {
                    let option = if name.ends_with(".fish") {
                        format!("-l {long}")
                    } else {
                        format!("--{long}")
                    };
                    assert!(script.contains(&option), "{option} missing in {name}");
                }
            }
        }
    }

    #[test]
    fn collect_and_hold_are_not_commands() {
        let cmd = parity_analyze::command();
        let names: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
        assert_eq!(names, ["compare", "packages", "ping"]);
    }
}
