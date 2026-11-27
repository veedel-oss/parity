//! Sensitive paths: `docs/architecture.md`, Collect mode details.

use std::collections::HashSet;
use std::os::fd::AsRawFd;

use crate::pattern::{self, Pattern, glob};
use crate::{fs, path, sys};

// Built in, always applied. Extra patterns from the CLI only add to it.
pub const BUILTIN: &[&str] = &[
    "/etc/shadow",
    "/etc/shadow-",
    "/etc/gshadow",
    "/etc/gshadow-",
    "/etc/security/opasswd",
    "/var/backups/shadow.bak",
    "/var/backups/gshadow.bak",
    "/etc/ssh/ssh_host_*_key",
    "/etc/ssl/private",
    "/etc/krb5.keytab",
    "/etc/ipsec.secrets",
    "/etc/ppp/chap-secrets",
    "/etc/ppp/pap-secrets",
    "/etc/wireguard",
    "/etc/NetworkManager/system-connections",
    "/etc/letsencrypt/archive",
    "/etc/letsencrypt/keys",
    "/root/.ssh",
    "/root/.gnupg",
    "/home/*/.ssh",
    "/home/*/.gnupg",
    "/var/lib/systemd/random-seed",
];

// Upper bound of paths looked at when collecting hard-linked sensitive
// files, so a huge sensitive directory cannot stall a request.
const LINK_SCAN_BUDGET: usize = 20_000;

pub const MAX_EXTRA: usize = 256;

// The built-in patterns, as [`Sensitive::new`] takes them.
pub fn builtin() -> Vec<Vec<u8>> {
    BUILTIN.iter().map(|p| p.as_bytes().to_vec()).collect()
}

pub struct Sensitive {
    patterns: Vec<Pattern>,
    // Device and inode of sensitive regular files with more than one link.
    linked: HashSet<(u64, u64)>,
}

impl Sensitive {
    // Builds the list from the patterns that always apply and the extra
    // patterns of a request, and looks for hard links to sensitive files.
    pub fn new(builtin: &[Vec<u8>], extra: &[Vec<u8>]) -> Result<Self, String> {
        let mut s = Self::from_patterns(builtin, extra)?;
        s.collect_linked();
        Ok(s)
    }

    // The patterns only, without looking for hard links: opens no file.
    fn from_patterns(builtin: &[Vec<u8>], extra: &[Vec<u8>]) -> Result<Self, String> {
        if extra.len() > MAX_EXTRA {
            return Err(format!("more than {MAX_EXTRA} sensitive patterns"));
        }
        let mut patterns = Vec::new();
        for p in builtin.iter().chain(extra) {
            patterns.push(Pattern::new(p).map_err(|e| format!("invalid sensitive pattern: {e}"))?);
        }
        Ok(Sensitive {
            patterns,
            linked: HashSet::new(),
        })
    }

    pub fn matches(&self, p: &[u8]) -> bool {
        pattern::any_covers(&self.patterns, p)
    }

    // Whether a file is a hard link of a sensitive file.
    pub fn is_link_of_sensitive(&self, st: &libc::stat) -> bool {
        st.st_nlink > 1 && self.linked.contains(&(st.st_dev, st.st_ino))
    }

    fn collect_linked(&mut self) {
        let mut budget = LINK_SCAN_BUDGET;
        let mut linked = HashSet::new();
        for pat in &self.patterns {
            // Expand the pattern component by component.
            let mut paths = vec![b"/".to_vec()];
            for comp in pat.components() {
                let mut next = Vec::new();
                for p in &paths {
                    if comp.contains(&b'*') {
                        let Ok(fd) = fs::open_dir(p) else { continue };
                        let Ok(names) = sys::read_dir_names(&fd) else {
                            continue;
                        };
                        budget = budget.saturating_sub(names.len());
                        next.extend(
                            names
                                .iter()
                                .filter(|n| glob(comp, n))
                                .map(|n| path::join(p, n)),
                        );
                    } else {
                        next.push(path::join(p, comp));
                    }
                }
                paths = next;
                if budget == 0 {
                    break;
                }
            }
            for p in paths {
                note_tree(&p, &mut linked, &mut budget);
            }
        }
        self.linked = linked;
    }
}

// Notes hard-linked regular files at and below `p`.
fn note_tree(p: &[u8], linked: &mut HashSet<(u64, u64)>, budget: &mut usize) {
    if *budget == 0 || p.len() > path::MAX_PATH {
        return;
    }
    *budget -= 1;
    let Ok((dir, name)) = fs::open_parent(p) else {
        return;
    };
    let Ok(st) = sys::fstatat(dir.as_raw_fd(), &name) else {
        return;
    };
    match st.st_mode & libc::S_IFMT {
        libc::S_IFREG if st.st_nlink > 1 => {
            linked.insert((st.st_dev, st.st_ino));
        }
        libc::S_IFDIR => {
            let Ok(fd) = sys::openat(dir.as_raw_fd(), &name, sys::DIR_FLAGS) else {
                return;
            };
            let Ok(names) = sys::read_dir_names(&fd) else {
                return;
            };
            for n in names {
                note_tree(&path::join(p, &n), linked, budget);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_patterns() {
        let s = Sensitive::from_patterns(&builtin(), &[]).unwrap();
        for p in [
            &b"/etc/shadow"[..],
            b"/etc/gshadow-",
            b"/etc/ssh/ssh_host_rsa_key",
            b"/etc/ssl/private",
            b"/etc/ssl/private/server.key",
            b"/root/.ssh/id_ed25519",
            b"/home/alice/.ssh/authorized_keys",
        ] {
            assert!(s.matches(p), "{}", String::from_utf8_lossy(p));
        }
        for p in [
            &b"/etc/passwd"[..],
            b"/etc/shadowx",
            b"/etc/ssh/ssh_host_rsa_key.pub",
            b"/etc/ssl",
            b"/etc/ssl/certs/x.pem",
            b"/home/alice",
            b"/home/alice/.bashrc",
            b"/",
        ] {
            assert!(!s.matches(p), "{}", String::from_utf8_lossy(p));
        }
    }

    #[test]
    fn extra_patterns_add() {
        let new = |extra: &[Vec<u8>]| Sensitive::from_patterns(&builtin(), extra);
        let s = new(&[b"/srv/*/secrets".to_vec()]).unwrap();
        assert!(s.matches(b"/srv/app/secrets/db"));
        assert!(s.matches(b"/etc/shadow"));
        assert!(new(&[b"relative".to_vec()]).is_err());
        assert!(new(&[b"/a/../b".to_vec()]).is_err());
        assert!(new(&vec![b"/x".to_vec(); MAX_EXTRA + 1]).is_err());
    }
}
