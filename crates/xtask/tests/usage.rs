//! Runs `xtask` with arguments it does not take.

use std::process::Command;

#[test]
fn other_arguments_are_a_usage_error() {
    for args in [
        &[][..],
        &["deb", "a", "b"],
        &["gen-docs", "a"],
        &["docs", "a"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("usage: cargo xtask deb [OUTPUT]"));
    }
}
