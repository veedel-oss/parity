//! Path patterns, for sensitive paths and excludes.
//!
//! A pattern is an absolute, normalised path whose components may contain
//! `*`, matching any run of bytes within one component. A pattern covers a
//! path if the path matches it or lies below a path that does. A pattern
//! without `*` covers the path itself and everything below it.

use crate::path;

pub struct Pattern {
    components: Vec<Vec<u8>>,
}

impl Pattern {
    pub fn new(p: &[u8]) -> Result<Self, String> {
        path::validate(p)?;
        Ok(Pattern {
            components: path::components(p).map(<[u8]>::to_vec).collect(),
        })
    }

    // Components of the pattern, each of them may contain `*`.
    pub fn components(&self) -> &[Vec<u8>] {
        &self.components
    }

    // `comps` are the components of a validated path.
    fn covers(&self, comps: &[&[u8]]) -> bool {
        self.components.len() <= comps.len()
            && self.components.iter().zip(comps).all(|(g, c)| glob(g, c))
    }
}

// Whether one of `patterns` covers `p`, a validated path.
pub fn any_covers(patterns: &[Pattern], p: &[u8]) -> bool {
    let comps: Vec<&[u8]> = path::components(p).collect();
    patterns.iter().any(|pat| pat.covers(&comps))
}

// Matches `name` against `pat`, where `*` matches any run of bytes.
pub fn glob(pat: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pat.len() && pat[p] == b'*' {
            star = Some((p, n));
            p += 1;
        } else if p < pat.len() && pat[p] == name[n] {
            p += 1;
            n += 1;
        } else if let Some((sp, sn)) = star {
            p = sp + 1;
            n = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&b| b == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globbing() {
        assert!(glob(b"ssh_host_*_key", b"ssh_host_ed25519_key"));
        assert!(!glob(b"ssh_host_*_key", b"ssh_host_ed25519_key.pub"));
        assert!(glob(b"*", b""));
        assert!(glob(b"a*b*c", b"aXXbYYc"));
        assert!(!glob(b"a*b*c", b"aXXbYY"));
        assert!(glob(b"exact", b"exact"));
        assert!(!glob(b"exact", b"exactly"));
    }

    fn covers(pattern: &str, p: &str) -> bool {
        any_covers(&[Pattern::new(pattern.as_bytes()).unwrap()], p.as_bytes())
    }

    #[test]
    fn covering() {
        assert!(covers("/var/lib/*/tmp", "/var/lib/app/tmp"));
        assert!(covers("/var/lib/*/tmp", "/var/lib/app/tmp/x/y"));
        assert!(!covers("/var/lib/*/tmp", "/var/lib/tmp"));
        assert!(!covers("/var/lib/*/tmp", "/var/lib/app/sub/tmp"));
        assert!(!covers("/var/lib/*/tmp", "/var/lib/app/tmpx"));
        assert!(!covers("/var/lib/*/tmp", "/var/lib/app"));
        // Without `*`: the path itself and everything below it.
        assert!(covers("/etc", "/etc"));
        assert!(covers("/etc", "/etc/x"));
        assert!(!covers("/etc", "/etcx"));
        assert!(!covers("/etc/x", "/etc"));
        assert!(covers("/", "/"));
        assert!(covers("/", "/etc"));
    }

    #[test]
    fn invalid_patterns() {
        for p in ["", "relative", "/a/../b", "/a/", "//a"] {
            assert!(Pattern::new(p.as_bytes()).is_err(), "{p}");
        }
    }
}
