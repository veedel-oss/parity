//! Attaching collect mode to the protocol channel of another SSH session.
//!
//! The CLI opens two sessions over one SSH connection. The first runs
//! `parity hold <nonce>` without privileges: it prints its PID and keeps the
//! session's stdin, stdout and stderr open without touching them. The second
//! runs `sudo parity collect --attach <pid> <nonce>` (or the same with su) on
//! a terminal, so sudo or su can ask for the password on the user's terminal
//! directly. Collect mode then opens the holder's stdin, stdout and stderr
//! through `/proc/<pid>/fd` and speaks the protocol there. The password never
//! passes through Parity, and the protocol never passes through a terminal.
//!
//! Collect mode only attaches to a holder that runs as the user who escalated
//! and was started with the same nonce. It opens the descriptors with that
//! user's real and effective UID and GID, so the kernel's own access check applies and
//! no `CAP_SYS_PTRACE` is needed.

use std::ffi::OsString;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};

use crate::sys;

pub fn valid_nonce(n: &str) -> bool {
    (16..=64).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_alphanumeric())
}

// Identity of the holder process from `/proc/<pid>/status`.
fn holder_ids(pid: u32) -> Result<(u32, u32), String> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|e| format!("holder process {pid} not found: {e}"))?;
    parse_ids(&status).ok_or_else(|| format!("holder process {pid} has mixed UIDs or GIDs"))
}

// Returns the UID and GID if real, effective, saved and file system IDs are
// all the same.
fn parse_ids(status: &str) -> Option<(u32, u32)> {
    let ids = |key: &str| -> Option<u32> {
        let line = status.lines().find(|l| l.starts_with(key))?;
        let v: Vec<u32> = line[key.len()..]
            .split_whitespace()
            .map(|x| x.parse().ok())
            .collect::<Option<_>>()?;
        (v.len() == 4 && v.iter().all(|&x| x == v[0])).then(|| v[0])
    };
    Some((ids("Uid:")?, ids("Gid:")?))
}

fn check_cmdline(pid: u32, nonce: &str) -> Result<(), String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline"))
        .map_err(|e| format!("holder process {pid} not found: {e}"))?;
    if cmdline_matches(&raw, nonce) {
        Ok(())
    } else {
        Err(format!(
            "process {pid} is not the Parity holder of this run"
        ))
    }
}

fn cmdline_matches(raw: &[u8], nonce: &str) -> bool {
    let args: Vec<&[u8]> = raw
        .strip_suffix(b"\0")
        .unwrap_or(raw)
        .split(|&b| b == 0)
        .collect();
    args.len() == 3 && args[1] == b"hold" && args[2] == nonce.as_bytes()
}

// UID of the user who escalated: `SUDO_UID` from sudo, otherwise the owner
// of the terminal (set by sshd to the login user), which covers su.
fn invoking_uid() -> Result<u32, String> {
    if let Some(uid) = std::env::var("SUDO_UID").ok().and_then(|v| v.parse().ok()) {
        return Ok(uid);
    }
    // SAFETY: isatty has no preconditions.
    if unsafe { libc::isatty(0) } == 1 {
        let st = sys::fstat(0).map_err(|e| format!("cannot stat the terminal: {e}"))?;
        return Ok(st.st_uid);
    }
    Err("cannot tell which user escalated (no SUDO_UID and no terminal)".into())
}

struct Ids {
    uid: u32,
    gid: u32,
}

// Opens the holder's descriptors with its UID and GID.
//
// Root may lack `CAP_SYS_PTRACE` (containers drop it), so the descriptors
// are opened as the holder's user instead. Real and effective IDs are both
// switched: before Linux 4.5, `/proc/<pid>/fd` checks the real IDs of the
// caller, from 4.5 on the file system IDs (`PTRACE_MODE_FSCREDS`). The
// saved IDs stay 0, which allows switching back.
fn open_as(ids: &Ids, pid: u32) -> Result<[OwnedFd; 3], String> {
    // SAFETY: plain system calls; we are root, so switching back succeeds.
    unsafe {
        if libc::setresgid(ids.gid, ids.gid, 0) != 0 {
            return Err(format!("setresgid: {}", io::Error::last_os_error()));
        }
        if libc::setresuid(ids.uid, ids.uid, 0) != 0 {
            let e = io::Error::last_os_error();
            if libc::setresgid(0, 0, 0) != 0 {
                libc::_exit(1);
            }
            return Err(format!("setresuid: {e}"));
        }
    }
    let open = |n: u32, flags: libc::c_int| {
        let p = sys::cstring(format!("/proc/{pid}/fd/{n}").as_bytes()).expect("no NUL");
        sys::open(&p, flags | libc::O_CLOEXEC | libc::O_NOCTTY)
            .map_err(|e| format!("cannot open fd {n} of the holder: {e}"))
    };
    let fds = (|| {
        Ok([
            open(0, libc::O_RDONLY)?,
            open(1, libc::O_WRONLY)?,
            open(2, libc::O_WRONLY)?,
        ])
    })();
    // SAFETY: switching back to root, which the saved UID allows.
    unsafe {
        if libc::setresuid(0, 0, 0) != 0 || libc::setresgid(0, 0, 0) != 0 {
            // Never carry on with mixed credentials.
            libc::_exit(1);
        }
    }
    fds
}

// Attaches to the holder and makes its descriptors stdin, stdout and
// stderr of a detached child. Returns in the child; the parent exits, which
// ends sudo or su and the terminal session.
pub fn attach(args: &[OsString]) -> Result<(), String> {
    let (pid, nonce) = match args {
        [pid, nonce] => (pid.to_str(), nonce.to_str()),
        _ => return Err("usage: parity collect --attach <pid> <nonce>".into()),
    };
    let pid: u32 = pid
        .and_then(|p| p.parse().ok())
        .filter(|&p| p > 1)
        .ok_or("invalid holder pid")?;
    let nonce = nonce.filter(|n| valid_nonce(n)).ok_or("invalid nonce")?;

    let expected = invoking_uid()?;
    let check = || -> Result<Ids, String> {
        let (uid, gid) = holder_ids(pid)?;
        if uid != expected {
            return Err(format!(
                "holder process {pid} belongs to another user than the one who escalated"
            ));
        }
        check_cmdline(pid, nonce)?;
        Ok(Ids { uid, gid })
    };
    let ids = check()?;
    let fds = open_as(&ids, pid)?;
    for fd in &fds {
        let st = sys::fstat(fd.as_raw_fd()).map_err(|e| e.to_string())?;
        if st.st_mode & libc::S_IFMT != libc::S_IFIFO {
            return Err("the holder's descriptors are not pipes".into());
        }
    }
    // The holder must still be the same process now that the files are open.
    check()?;

    // SAFETY: single-threaded at this point; standard daemon steps. SIGHUP is
    // ignored before the fork so the hangup of the terminal session cannot
    // reach the child before it has its own session.
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        match libc::fork() {
            -1 => return Err(format!("fork: {}", io::Error::last_os_error())),
            0 => {}
            _ => libc::_exit(0),
        }
        libc::setsid();
        for (i, fd) in fds.iter().enumerate() {
            if libc::dup2(fd.as_raw_fd(), i as libc::c_int) == -1 {
                libc::_exit(1);
            }
        }
    }
    drop(fds);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce() {
        assert!(valid_nonce("abcdefghijklmnop"));
        assert!(!valid_nonce("short"));
        assert!(!valid_nonce("abcdefghijklmnop;rm"));
        assert!(!valid_nonce(&"a".repeat(65)));
    }

    #[test]
    fn ids() {
        let s = "Name:\tparity\nUid:\t1000\t1000\t1000\t1000\nGid:\t100\t100\t100\t100\n";
        assert_eq!(parse_ids(s), Some((1000, 100)));
        let s = "Uid:\t1000\t0\t1000\t1000\nGid:\t100\t100\t100\t100\n";
        assert_eq!(parse_ids(s), None);
        assert_eq!(parse_ids("Uid:\t1\t1\t1\t1\n"), None);
    }

    // A different nonce in every run, like the CLI's: 16 hex digits from the
    // standard library's randomly seeded hasher. For tests only, not for
    // security (collect mode itself never makes nonces).
    fn test_nonce() -> String {
        use std::hash::{BuildHasher, RandomState};
        format!("{:016x}", RandomState::new().hash_one(0u8))
    }

    #[test]
    fn cmdline() {
        let test_nonce = test_nonce();
        assert!(valid_nonce(&test_nonce));
        let cmdline = |s: &str| s.replace(' ', "\0") + "\0";
        assert!(cmdline_matches(
            cmdline(&format!("/usr/bin/parity hold {test_nonce}")).as_bytes(),
            &test_nonce
        ));
        assert!(!cmdline_matches(
            cmdline("/usr/bin/parity hold other").as_bytes(),
            &test_nonce
        ));
        assert!(!cmdline_matches(
            cmdline(&format!("sleep {test_nonce}")).as_bytes(),
            &test_nonce
        ));
        assert!(!cmdline_matches(
            cmdline(&format!("x hold {test_nonce} extra")).as_bytes(),
            &test_nonce
        ));
    }
}
