//! Validation of paths received from the CLI.

pub const MAX_PATH: usize = 4096;

// Accepts only absolute, normalised paths: no empty, `.` or `..`
// components, no trailing slash (except for `/` itself), no NUL bytes.
pub fn validate(p: &[u8]) -> Result<(), String> {
    let show = String::from_utf8_lossy(p);
    if p.len() > MAX_PATH {
        return Err(format!("path too long: {show}"));
    }
    if p.first() != Some(&b'/') {
        return Err(format!("path is not absolute: {show}"));
    }
    if p.contains(&0) {
        return Err("path contains a NUL byte".into());
    }
    if p == b"/" {
        return Ok(());
    }
    for c in p[1..].split(|&b| b == b'/') {
        if c.is_empty() || c == b"." || c == b".." {
            return Err(format!("path is not normalised: {show}"));
        }
    }
    Ok(())
}

// Components of a validated path; empty for `/`.
pub fn components(p: &[u8]) -> impl Iterator<Item = &[u8]> {
    p[1..].split(|&b| b == b'/').filter(|c| !c.is_empty())
}

pub fn join(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(parent.len() + 1 + name.len());
    p.extend_from_slice(parent);
    if parent != b"/" {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_paths() {
        for p in [
            &b"/"[..],
            b"/etc",
            b"/etc/ssh/sshd_config",
            b"/a/.hidden",
            b"/a/..b",
        ] {
            assert!(validate(p).is_ok(), "{p:?}");
        }
    }

    #[test]
    fn invalid_paths() {
        for p in [
            &b""[..],
            b"etc",
            b"//etc",
            b"/etc/",
            b"/etc/../shadow",
            b"/etc/./shadow",
            b"/etc//shadow",
            b"/etc/sh\0adow",
            b"/..",
        ] {
            assert!(validate(p).is_err(), "{p:?}");
        }
        assert!(validate(&[b'/'; MAX_PATH + 1]).is_err());
    }

    #[test]
    fn join_and_components() {
        assert_eq!(join(b"/", b"etc"), b"/etc");
        assert_eq!(join(b"/etc", b"x"), b"/etc/x");
        assert_eq!(components(b"/").count(), 0);
        assert_eq!(components(b"/a/b").collect::<Vec<_>>(), vec![b"a", b"b"]);
    }
}
