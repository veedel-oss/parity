//! Release tool of Parity, never shipped and not linked into `parity`.
//! `cargo xtask` is an alias for `cargo run --locked -p xtask --`
//! (`.cargo/config.toml`).
//!
//! `cargo xtask gen-docs OUTDIR` writes the man pages and shell completions
//! of the Debian package, rendered from the CLI's clap definition, so they
//! always match the help texts:
//!
//! - `OUTDIR/man/parity.1` and one page per command (`parity-compare.1`, ...)
//! - `OUTDIR/completions/parity.bash`, `_parity` (zsh) and `parity.fish`
//!
//! `cargo xtask deb [OUTPUT]` builds the Debian package as the release does
//! (`.github/workflows/release.yml`): the static binary, the man pages and
//! completions in `packaging/deb/generated`, then cargo-deb on that binary
//! (`[package.metadata.deb]` in `crates/parity/Cargo.toml`). It needs the
//! cargo-deb version pinned in `packaging/deb/tools/Cargo.toml`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Command;
use clap_complete::Shell;
use clap_mangen::Man;

const USAGE: &str = "usage: cargo xtask gen-docs OUTDIR\n       cargo xtask deb [OUTPUT]";

// The only target that is released.
const TARGET: &str = "x86_64-unknown-linux-musl";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [task, out] if task == "gen-docs" => gen_docs(Path::new(out)).map_err(|e| e.to_string()),
        [task] if task == "deb" => deb(None),
        [task, out] if task == "deb" => deb(Some(Path::new(out))),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

// The root of the workspace, two levels above this crate.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// Runs cargo with `args` in the workspace root. The cargo that runs xtask,
// so the toolchain of rust-toolchain.toml.
fn cargo(args: &[&str], env: &[(&str, OsString)]) -> Result<(), String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = std::process::Command::new(&cargo)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(workspace_root())
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo {} failed ({status})", args.join(" ")))
    }
}

// Builds the Debian package, to `out` (a file or a directory) or to
// cargo-deb's default, target/debian.
fn deb(out: Option<&Path>) -> Result<(), String> {
    let root = workspace_root();
    let tools = root.join("packaging/deb/tools/Cargo.toml");
    let manifest =
        std::fs::read_to_string(&tools).map_err(|e| format!("{}: {e}", tools.display()))?;
    let pinned = pinned_cargo_deb(&manifest)
        .ok_or("packaging/deb/tools/Cargo.toml pins no exact version of cargo-deb")?;
    let installed = std::process::Command::new("cargo-deb")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| installed_cargo_deb(&String::from_utf8_lossy(&o.stdout)).map(str::to_owned));
    if installed.as_deref() != Some(pinned) {
        let found = installed.map_or("none".into(), |v| format!("cargo-deb {v}"));
        return Err(format!(
            "the package needs cargo-deb {pinned} (packaging/deb/tools/Cargo.toml), found {found}. Install it with\n\n    cargo install cargo-deb --version ={pinned} --locked"
        ));
    }

    // The same command as the release, so the same binary.
    cargo(&["build", "--release", "--locked", "--target", TARGET], &[])?;

    // Generated afresh, so no page of a former command stays behind.
    let generated = root.join("packaging/deb/generated");
    if generated.exists() {
        std::fs::remove_dir_all(&generated).map_err(|e| format!("{}: {e}", generated.display()))?;
    }
    gen_docs(&generated).map_err(|e| format!("{}: {e}", generated.display()))?;

    // The file times in the package: SOURCE_DATE_EPOCH if set, otherwise the
    // time of the commit, so the same commit gives the same package.
    let mut env = Vec::new();
    if std::env::var_os("SOURCE_DATE_EPOCH").is_none() {
        if let Some(time) = commit_time(&root) {
            env.push(("SOURCE_DATE_EPOCH", time.into()));
        }
    }
    let mut args = vec![
        "deb",
        "--locked",
        "-p",
        "parity",
        "--target",
        TARGET,
        "--no-build",
        "--no-strip",
        "--compress-type",
        "xz",
    ];
    // Relative to where xtask was started, not to the workspace root.
    let out = out.map(|o| std::path::absolute(o).map_err(|e| format!("{}: {e}", o.display())));
    let out = out.transpose()?;
    let out = out.as_ref().map(|o| o.to_string_lossy().into_owned());
    if let Some(out) = &out {
        args.extend(["--output", out]);
    }
    cargo(&args, &env)
}

// The version of cargo-deb that the tool manifest pins, if it pins an exact one.
fn pinned_cargo_deb(manifest: &str) -> Option<&str> {
    let line = manifest.lines().find(|l| l.starts_with("cargo-deb ="))?;
    let version = line.split('"').nth(1)?.strip_prefix('=')?;
    let parts: Vec<&str> = version.split('.').collect();
    (parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())))
    .then_some(version)
}

// The version in the output of `cargo-deb --version`.
fn installed_cargo_deb(output: &str) -> Option<&str> {
    output.trim().strip_prefix("cargo-deb ")
}

// The commit time of HEAD in seconds, if git can tell.
fn commit_time(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["log", "-1", "--format=%ct"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let time = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!time.is_empty() && time.bytes().all(|b| b.is_ascii_digit())).then_some(time)
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
    fn the_tool_manifest_pins_cargo_deb() {
        let manifest = include_str!("../../../packaging/deb/tools/Cargo.toml");
        let version = pinned_cargo_deb(manifest).unwrap();
        assert_eq!(version.split('.').count(), 3);
    }

    #[test]
    fn only_an_exact_version_counts_as_pinned() {
        assert_eq!(pinned_cargo_deb("cargo-deb = \"=3.8.0\"\n"), Some("3.8.0"));
        for req in ["3.8.0", "^3.8.0", "=3.8", "=3.8.x", "=3..0", ">=3.8.0"] {
            assert_eq!(
                pinned_cargo_deb(&format!("cargo-deb = \"{req}\"\n")),
                None,
                "{req}"
            );
        }
        assert_eq!(pinned_cargo_deb("# cargo-deb = \"=3.8.0\"\n"), None);
    }

    #[test]
    fn version_of_the_installed_cargo_deb() {
        assert_eq!(installed_cargo_deb("cargo-deb 3.8.0\n"), Some("3.8.0"));
        assert_eq!(installed_cargo_deb("something else 1.0\n"), None);
    }

    #[test]
    fn collect_and_hold_are_not_commands() {
        let cmd = parity_analyze::command();
        let names: Vec<&str> = cmd.get_subcommands().map(|s| s.get_name()).collect();
        assert_eq!(names, ["compare", "packages", "ping"]);
    }
}
