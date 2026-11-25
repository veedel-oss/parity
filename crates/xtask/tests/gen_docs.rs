//! Runs `xtask gen-docs` into a directory the test creates.

use std::path::PathBuf;
use std::process::Command;

fn xtask() -> Command {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
}

#[test]
fn writes_the_man_pages_and_completions() {
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gen-docs");
    let _ = std::fs::remove_dir_all(&out);
    let status = xtask().arg("gen-docs").arg(&out).status().unwrap();
    assert!(status.success());
    let mut files: Vec<String> = ["man", "completions"]
        .iter()
        .flat_map(|d| std::fs::read_dir(out.join(d)).unwrap())
        .map(|e| {
            let path = e.unwrap().path();
            path.strip_prefix(&out).unwrap().display().to_string()
        })
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "completions/_parity",
            "completions/parity.bash",
            "completions/parity.fish",
            "man/parity-compare.1",
            "man/parity-packages.1",
            "man/parity-ping.1",
            "man/parity.1",
        ]
    );
    std::fs::remove_dir_all(&out).unwrap();
}

#[test]
fn other_arguments_are_a_usage_error() {
    for args in [
        &[][..],
        &["gen-docs"],
        &["gen-docs", "a", "b"],
        &["docs", "a"],
    ] {
        let out = xtask().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("usage: cargo run -p xtask -- gen-docs OUTDIR")
        );
    }
}
