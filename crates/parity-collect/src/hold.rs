//! `parity hold <nonce>`, the unprivileged half of an escalated session:
//! `docs/architecture.md`, Connection model.

use std::ffi::OsString;
use std::io::Write;

use crate::attach::valid_nonce;

pub const PREFIX: &str = "PARITY-HOLD";

// Not unit tested: the holder process itself, e2e tests.
#[cfg_attr(test, mutants::skip)]
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

// Owner and mode of one path on the way from the binary up to `/`.
struct PathInfo {
    path: std::path::PathBuf,
    uid: u32,
    mode: u32,
    is_dir: bool,
}

// The CLI runs this very binary as root next. Refuse if a third user could
// replace it.
// Not unit tested: reads the real path of the binary; check_chain has the rules and is tested.
#[cfg_attr(test, mutants::skip)]
fn check_safe_path(exe: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let mut chain = Vec::new();
    for cur in exe.ancestors() {
        let m =
            std::fs::metadata(cur).map_err(|e| format!("cannot check {}: {e}", cur.display()))?;
        chain.push(PathInfo {
            path: cur.to_path_buf(),
            uid: m.uid(),
            mode: m.mode() & 0o7777,
            is_dir: m.is_dir(),
        });
    }
    // SAFETY: getuid has no preconditions.
    check_chain(exe, &chain, unsafe { libc::getuid() })
}

// The rules: docs/collect-mode.md, The binary run as root.
fn check_chain(exe: &std::path::Path, chain: &[PathInfo], uid: u32) -> Result<(), String> {
    for p in chain {
        let writable = p.mode & 0o002 != 0;
        let sticky_dir = p.is_dir && p.mode & 0o1000 != 0;
        // The reason names no UID (see require_root); `ls -l` shows the owner.
        let reason = if p.uid != 0 && p.uid != uid {
            "it belongs to neither root nor you".to_string()
        } else if writable && !sticky_dir {
            format!("it is writable by others (mode {:04o})", p.mode)
        } else {
            continue;
        };
        return Err(format!(
            "refusing to run {} as root: {} can be changed by other users: {reason}",
            exe.display(),
            p.path.display()
        ));
    }
    Ok(())
}

// Waits until stdin has no writer left or stdout has no reader left. Polling
// with no requested events reports only these conditions, so no data on
// stdin is consumed.
// Not unit tested: waits for a real ssh session to end, e2e tests.
#[cfg_attr(test, mutants::skip)]
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
    use std::path::Path;

    const USER: u32 = 1000;

    // The chain of /usr/local/bin/parity with the given owner and mode of
    // the binary and of the directory it is in.
    fn chain(file: (u32, u32), dir: (u32, u32)) -> Vec<PathInfo> {
        let info = |path: &str, (uid, mode): (u32, u32), is_dir| PathInfo {
            path: path.into(),
            uid,
            mode,
            is_dir,
        };
        vec![
            info("/usr/local/bin/parity", file, false),
            info("/usr/local/bin", dir, true),
            info("/usr/local", (0, 0o755), true),
            info("/usr", (0, 0o755), true),
            info("/", (0, 0o755), true),
        ]
    }

    fn check(file: (u32, u32), dir: (u32, u32)) -> Result<(), String> {
        check_chain(Path::new("/usr/local/bin/parity"), &chain(file, dir), USER)
    }

    #[test]
    fn owners() {
        assert!(check((0, 0o755), (0, 0o755)).is_ok());
        assert!(check((USER, 0o755), (USER, 0o755)).is_ok());
        let e = check((1001, 0o755), (0, 0o755)).unwrap_err();
        assert!(
            e.contains(
                "/usr/local/bin/parity can be changed by other users: it belongs to neither root nor you"
            ),
            "{e}"
        );
        assert!(!e.contains("1001"), "names no UID: {e}");
        assert!(check((0, 0o755), (1001, 0o755)).is_err());
    }

    #[test]
    fn modes() {
        // Group write: Debian's /usr/local/bin is root:staff 2775.
        assert!(check((0, 0o775), (0, 0o2775)).is_ok());
        // Writable by others.
        assert!(check((0, 0o757), (0, 0o755)).is_err());
        let e = check((0, 0o755), (0, 0o777)).unwrap_err();
        assert!(
            e.contains(
                "/usr/local/bin can be changed by other users: it is writable by others (mode 0777)"
            ),
            "{e}"
        );
        // A sticky directory protects its entries, a sticky file nothing.
        assert!(check((0, 0o755), (0, 0o1777)).is_ok());
        assert!(check((0, 0o1757), (0, 0o755)).is_err());
    }

    #[test]
    fn whole_chain_is_checked() {
        let mut c = chain((0, 0o755), (0, 0o755));
        c[4].mode = 0o777;
        let e = check_chain(Path::new("/usr/local/bin/parity"), &c, USER).unwrap_err();
        assert!(e.contains(": / can be changed"), "{e}");
    }
}
