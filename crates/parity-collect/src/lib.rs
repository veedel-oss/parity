//! Collect mode: runs as root on the target, reads requests from stdin,
//! answers on stdout, logs to stderr and exits. Read-only (AGENTS.md).

use std::ffi::OsString;
use std::io::{BufReader, BufWriter};
use std::os::fd::{AsFd, OwnedFd};

mod attach;
mod fs;
pub mod hold;
mod path;
mod pattern;
pub mod sensitive;
pub mod session;
mod sys;
mod walk;

pub use attach::valid_nonce;

// Takes the effective UID, so tests can check both outcomes. Messages name
// no UID: output gets passed on, and CodeQL counts UIDs as account data.
pub fn require_root(euid: u32) -> Result<(), String> {
    if euid == 0 {
        Ok(())
    } else {
        Err("must run as root (effective UID 0); the parity CLI starts it with sudo or su".into())
    }
}

// Called by the binary before any other argument parsing.
// Not unit tested: the exit code of a real process, e2e tests.
#[cfg_attr(test, mutants::skip)]
pub fn main(args: &[OsString]) -> i32 {
    match run(args, sys::euid()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("parity collect: {e}");
            1
        }
    }
}

fn run(args: &[OsString], euid: u32) -> Result<(), String> {
    require_root(euid)?;
    match args.first().and_then(|a| a.to_str()) {
        None => {}
        Some("--attach") => attach::attach(&args[1..])?,
        Some(_) => {
            return Err("collect mode is started by the parity CLI; it takes no options".into());
        }
    }
    sys::raise_fd_limit();
    let dup = |fd: std::os::fd::BorrowedFd| -> Result<OwnedFd, String> {
        fd.try_clone_to_owned().map_err(|e| e.to_string())
    };
    let stdin = dup(std::io::stdin().as_fd())?;
    let stdout = dup(std::io::stdout().as_fd())?;
    let mut input = BufReader::new(std::fs::File::from(stdin));
    let mut output = BufWriter::new(std::fs::File::from(stdout));
    session::run(&mut input, &mut output, &session::Config::target()).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_check() {
        assert!(require_root(0).is_ok());
        let e = require_root(1000).unwrap_err();
        assert!(e.contains("must run as root"), "{e}");
        assert!(!e.contains("1000"), "names no UID: {e}");
    }

    #[test]
    fn refuses_without_root() {
        assert!(run(&[], 1000).is_err());
    }
}
