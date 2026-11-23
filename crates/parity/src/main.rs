//! The `parity` binary. Decides the role before any other argument parsing:
//! `collect` (and its unprivileged helper `hold`) hand over to collect mode
//! directly, so no CLI code ever runs as root. Everything else is the CLI.

use std::ffi::OsString;
use std::process::exit;

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let code = match args.first().and_then(|a| a.to_str()) {
        Some("collect") => parity_collect::main(&args[1..]),
        Some("hold") => parity_collect::hold::main(&args[1..]),
        _ => parity_analyze::main(args),
    };
    exit(code);
}
