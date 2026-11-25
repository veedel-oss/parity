//! Release tool of Parity, never shipped and not linked into `parity`.
//! `cargo xtask` is an alias for `cargo run --locked -p xtask --`
//! (`.cargo/config.toml`).
//!
//! `cargo xtask deb [OUTPUT]` builds the Debian package as the release does
//! (`.github/workflows/release.yml`): the static binary, the shell
//! completions in `packaging/deb/generated` (the scripts that
//! `parity completion SHELL` prints), then cargo-deb on that binary
//! (`[package.metadata.deb]` in `crates/parity/Cargo.toml`). It needs the
//! cargo-deb version pinned in `packaging/deb/tools/Cargo.toml`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use parity_analyze::Shell;

const USAGE: &str = "usage: cargo xtask deb [OUTPUT]";

// The only target that is released.
const TARGET: &str = "x86_64-unknown-linux-musl";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
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

    // Generated afresh, so no file of a former build stays behind.
    let generated = root.join("packaging/deb/generated");
    write_completions(&generated).map_err(|e| format!("{}: {e}", generated.display()))?;

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

// The completions of the package for bash, fish and zsh, as file name and
// script: what `parity completion SHELL` prints, from the same function.
fn completions() -> Vec<(&'static str, Vec<u8>)> {
    [
        (Shell::Bash, "parity.bash"),
        (Shell::Fish, "parity.fish"),
        (Shell::Zsh, "_parity"),
    ]
    .into_iter()
    .map(|(shell, name)| (name, parity_analyze::completion(shell)))
    .collect()
}

// Writes the completions into `out/completions`, after removing `out`.
fn write_completions(out: &Path) -> std::io::Result<()> {
    if out.exists() {
        std::fs::remove_dir_all(out)?;
    }
    let dir = out.join("completions");
    std::fs::create_dir_all(&dir)?;
    for (name, script) in completions() {
        std::fs::write(dir.join(name), script)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_file_has_the_script_of_its_shell() {
        let scripts = completions();
        let names: Vec<&str> = scripts.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["parity.bash", "parity.fish", "_parity"]);
        // How each shell registers the completion of the command parity.
        for ((name, script), registers) in scripts.iter().zip([
            "complete -F _parity",
            "complete -c parity",
            "#compdef parity",
        ]) {
            let script = std::str::from_utf8(script).unwrap();
            assert!(script.contains(registers), "{name}");
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
}
