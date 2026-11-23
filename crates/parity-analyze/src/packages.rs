//! Debian packages: parsing the dpkg status database and comparing lists.

use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Package {
    pub name: String,
    pub arch: String,
    pub version: String,
    // dpkg state, `installed` for a normally installed package.
    pub state: String,
}

// Parses `/var/lib/dpkg/status`. Packages in state `not-installed` are left
// out. Sorted by name and architecture.
pub fn parse_status(data: &[u8]) -> Vec<Package> {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    for stanza in text.split("\n\n") {
        let mut name = None;
        let mut arch = String::new();
        let mut version = String::new();
        let mut state = String::new();
        for line in stanza.lines() {
            if line.starts_with([' ', '\t']) {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key {
                "Package" => name = Some(value.to_string()),
                "Architecture" => arch = value.to_string(),
                "Version" => version = value.to_string(),
                "Status" => state = value.split_whitespace().nth(2).unwrap_or("").to_string(),
                _ => {}
            }
        }
        if let Some(name) = name {
            if state != "not-installed" {
                out.push(Package {
                    name,
                    arch,
                    version,
                    state,
                });
            }
        }
    }
    out.sort_by(|a, b| (&a.name, &a.arch).cmp(&(&b.name, &b.arch)));
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageChange {
    pub name: String,
    pub arch: String,
    pub a: Side,
    pub b: Side,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Side {
    pub version: String,
    pub state: String,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct PackageDiff {
    pub only_a: Vec<Package>,
    pub only_b: Vec<Package>,
    pub changed: Vec<PackageChange>,
}

impl PackageDiff {
    pub fn is_empty(&self) -> bool {
        self.only_a.is_empty() && self.only_b.is_empty() && self.changed.is_empty()
    }
}

pub fn compare(a: &[Package], b: &[Package]) -> PackageDiff {
    let key = |p: &Package| (p.name.clone(), p.arch.clone());
    let am: BTreeMap<_, _> = a.iter().map(|p| (key(p), p)).collect();
    let bm: BTreeMap<_, _> = b.iter().map(|p| (key(p), p)).collect();
    let mut d = PackageDiff::default();
    for (k, pa) in &am {
        match bm.get(k) {
            None => d.only_a.push((*pa).clone()),
            Some(pb) if pa.version != pb.version || pa.state != pb.state => {
                d.changed.push(PackageChange {
                    name: pa.name.clone(),
                    arch: pa.arch.clone(),
                    a: Side {
                        version: pa.version.clone(),
                        state: pa.state.clone(),
                    },
                    b: Side {
                        version: pb.version.clone(),
                        state: pb.state.clone(),
                    },
                })
            }
            Some(_) => {}
        }
    }
    for (k, pb) in &bm {
        if !am.contains_key(k) {
            d.only_b.push((*pb).clone());
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "\
Package: bash
Status: install ok installed
Priority: required
Architecture: amd64
Version: 5.2.15-2+b2
Description: GNU Bourne Again SHell
 Bash is an sh-compatible command language interpreter.
 .
 Continuation: not a field

Package: libc6
Status: install ok installed
Architecture: i386
Multi-Arch: same
Version: 2.36-9

Package: libc6
Status: install ok installed
Architecture: amd64
Version: 2.36-9

Package: old
Status: deinstall ok config-files
Architecture: all
Version: 1.0

Package: gone
Status: purge ok not-installed
Architecture: all
";

    fn pkg(name: &str, arch: &str, version: &str, state: &str) -> Package {
        Package {
            name: name.into(),
            arch: arch.into(),
            version: version.into(),
            state: state.into(),
        }
    }

    #[test]
    fn parses_status() {
        let p = parse_status(STATUS.as_bytes());
        assert_eq!(
            p,
            vec![
                pkg("bash", "amd64", "5.2.15-2+b2", "installed"),
                pkg("libc6", "amd64", "2.36-9", "installed"),
                pkg("libc6", "i386", "2.36-9", "installed"),
                pkg("old", "all", "1.0", "config-files"),
            ]
        );
    }

    #[test]
    fn compares() {
        let a = vec![
            pkg("bash", "amd64", "5.2", "installed"),
            pkg("curl", "amd64", "8.0", "installed"),
            pkg("vim", "amd64", "9.0", "installed"),
        ];
        let b = vec![
            pkg("bash", "amd64", "5.2", "installed"),
            pkg("curl", "amd64", "8.1", "installed"),
            pkg("nano", "amd64", "7.0", "installed"),
        ];
        let d = compare(&a, &b);
        assert_eq!(d.only_a, vec![pkg("vim", "amd64", "9.0", "installed")]);
        assert_eq!(d.only_b, vec![pkg("nano", "amd64", "7.0", "installed")]);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].name, "curl");
        assert_eq!(
            (
                d.changed[0].a.version.as_str(),
                d.changed[0].b.version.as_str()
            ),
            ("8.0", "8.1")
        );
        assert!(compare(&a, &a).is_empty());
    }
}
