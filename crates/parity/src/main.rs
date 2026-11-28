//! Hands `collect` and `hold` to collect mode before any argument parsing, so
//! no CLI code runs as root.

use std::ffi::OsString;
use std::process::exit;

// Not unit tested: the dispatch of a real process, e2e tests.
#[cfg_attr(test, mutants::skip)]
fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let code = match args.first().and_then(|a| a.to_str()) {
        Some("collect") => parity_collect::main(&args[1..]),
        Some("hold") => parity_collect::hold::main(&args[1..]),
        _ => parity_analyze::main(args),
    };
    exit(code);
}
