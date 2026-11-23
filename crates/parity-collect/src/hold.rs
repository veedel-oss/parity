//! `parity hold <nonce>`: the unprivileged half of an escalated session (see
//! [`crate::attach`]). Prints one line and then keeps stdin, stdout and
//! stderr open without reading or writing them, until the SSH session ends.

use std::ffi::OsString;
use std::io::Write;

use crate::attach::valid_nonce;

// Prefix of the line the holder prints. The line continues with the
// protocol version, the program version, the PID and the path of the binary
// in hex, separated by spaces.
pub const PREFIX: &str = "PARITY-HOLD";

pub fn main(args: &[OsString]) -> i32 {
    let nonce = match args {
        [n] => n.to_str().filter(|n| valid_nonce(n)),
        _ => None,
    };
    if nonce.is_none() {
        eprintln!("parity hold: started by the parity CLI, not meant to be run by hand");
        return 2;
    }
    let exe = match std::fs::read_link("/proc/self/exe") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("parity hold: cannot find own binary: {e}");
            return 1;
        }
    };
    if let Err(e) = check_safe_path(&exe) {
        eprintln!("parity hold: {e}");
        return 3;
    }
    let exe = std::os::unix::ffi::OsStrExt::as_bytes(exe.as_os_str()).to_vec();
    let hex: String = exe.iter().map(|b| format!("{b:02x}")).collect();
    let line = format!(
        "{PREFIX} {} {} {} {hex}\n",
        parity_proto::PROTOCOL_VERSION,
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    let mut out = std::io::stdout().lock();
    if out
        .write_all(line.as_bytes())
        .and_then(|_| out.flush())
        .is_err()
    {
        return 1;
    }
    drop(out);
    wait_for_hangup();
    0
}

// The CLI runs this very binary as root next. Refuse if a third user could
// replace it: the binary and every directory above it must belong to root
// or the user and must not be writable by others (directories with the
// sticky bit excepted, there only the owner of an entry can replace it).
// Group write is accepted: Debian ships `/usr/local/bin` as `root:staff`
// 2775, and sudo's `secure_path` trusts it the same way.
fn check_safe_path(exe: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let mut p = Some(exe);
    while let Some(cur) = p {
        let m =
            std::fs::metadata(cur).map_err(|e| format!("cannot check {}: {e}", cur.display()))?;
        let writable = m.mode() & 0o002 != 0;
        let sticky_dir = m.is_dir() && m.mode() & 0o1000 != 0;
        if (m.uid() != 0 && m.uid() != uid) || (writable && !sticky_dir) {
            return Err(format!(
                "refusing to run {} as root: {} can be changed by other users (owner {}, mode {:04o})",
                exe.display(),
                cur.display(),
                m.uid(),
                m.mode() & 0o7777
            ));
        }
        p = cur.parent();
    }
    Ok(())
}

// Waits until stdin has no writer left or stdout has no reader left. Polling
// with no requested events reports only these conditions, so no data on
// stdin is consumed.
fn wait_for_hangup() {
    let mut fds = [
        libc::pollfd {
            fd: 0,
            events: 0,
            revents: 0,
        },
        libc::pollfd {
            fd: 1,
            events: 0,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: fds is a valid array of pollfd.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if r > 0 {
            return;
        }
        if r < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_paths() {
        assert!(check_safe_path(std::path::Path::new("/")).is_ok());
        let dir = std::env::temp_dir().join(format!("parity-hold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("parity");
        std::fs::write(&f, b"").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The temp dir itself is world-writable but sticky.
        let ok = check_safe_path(&f);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o775)).unwrap();
        let group = check_safe_path(&f);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o757)).unwrap();
        let bad_file = check_safe_path(&f);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let bad_dir = check_safe_path(&f);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(ok.is_ok(), "{ok:?}");
        assert!(group.is_ok(), "{group:?}");
        assert!(
            bad_file
                .unwrap_err()
                .contains("can be changed by other users")
        );
        assert!(bad_dir.is_err());
    }
}
