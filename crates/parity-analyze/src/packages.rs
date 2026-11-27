//! Debian packages: parsing the dpkg database and comparing lists.

use std::collections::BTreeMap;

use serde::Serialize;

// The `Status` field of a package: what is wanted, an error flag and the
// current state, for example `install ok installed` or `hold ok installed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub want: String,
    pub flag: String,
    pub state: String,
}

impl Status {
    fn parse(value: &str) -> Status {
        let mut w = value.split_whitespace().map(str::to_string);
        Status {
            want: w.next().unwrap_or_default(),
            flag: w.next().unwrap_or_default(),
            state: w.next().unwrap_or_default(),
        }
    }

    // The parts that differ from a normally installed package (`install ok
    // installed`), for example "hold" or "deinstall, config-files".
    pub fn note(&self) -> Option<String> {
        let parts: Vec<&str> = [
            (self.want.as_str(), "install"),
            (self.flag.as_str(), "ok"),
            (self.state.as_str(), "installed"),
        ]
        .iter()
        .filter(|(v, normal)| v != normal)
        .map(|(v, _)| *v)
        .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }

    // Two or three letters as `dpkg-query -f '${db:Status-Abbrev}'` and
    // `dpkg -l` show them, for example `ii`, `hi`, `rc` or `iUR`.
    pub fn abbrev(&self) -> String {
        let want = match self.want.as_str() {
            "install" => 'i',
            "hold" => 'h',
            "deinstall" => 'r',
            "purge" => 'p',
            _ => 'u',
        };
        let state = match self.state.as_str() {
            "installed" => 'i',
            "config-files" => 'c',
            "half-installed" => 'H',
            "unpacked" => 'U',
            "half-configured" => 'F',
            "triggers-awaited" => 'W',
            "triggers-pending" => 't',
            _ => 'n',
        };
        let mut s = format!("{want}{state}");
        if self.flag == "reinstreq" {
            s.push('R');
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Package {
    pub name: String,
    pub arch: String,
    pub version: String,
    pub status: Status,
}

// Path of the dpkg status database and of its journal.
pub const STATUS_PATH: &[u8] = b"/var/lib/dpkg/status";
pub const UPDATES_DIR: &[u8] = b"/var/lib/dpkg/updates";

// Whether a file in the journal directory is a journal entry. dpkg only
// reads files whose names consist of digits, in name order.
pub fn is_journal_entry(name: &[u8]) -> bool {
    !name.is_empty() && name.iter().all(u8::is_ascii_digit)
}

// As dpkg-query sees them: each journal entry, in order, replaces the record
// of its package. Without `not-installed`, sorted by name and architecture.
pub fn parse_database(status: &[u8], updates: &[Vec<u8>]) -> Vec<Package> {
    let mut db = BTreeMap::new();
    for data in std::iter::once(status).chain(updates.iter().map(Vec::as_slice)) {
        for p in parse_records(data) {
            db.insert((p.name.clone(), p.arch.clone()), p);
        }
    }
    db.into_values()
        .filter(|p| p.status.state != "not-installed")
        .collect()
}

fn parse_records(data: &[u8]) -> Vec<Package> {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    for stanza in text.split("\n\n") {
        let mut name = None;
        let mut arch = String::new();
        let mut version = String::new();
        let mut status = Status::parse("");
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
                "Status" => status = Status::parse(value),
                _ => {}
            }
        }
        if let Some(name) = name {
            out.push(Package {
                name,
                arch,
                version,
                status,
            });
        }
    }
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
    pub status: Status,
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
            Some(pb) if pa.version != pb.version || pa.status != pb.status => {
                d.changed.push(PackageChange {
                    name: pa.name.clone(),
                    arch: pa.arch.clone(),
                    a: Side {
                        version: pa.version.clone(),
                        status: pa.status.clone(),
                    },
                    b: Side {
                        version: pb.version.clone(),
                        status: pb.status.clone(),
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

    fn pkg(name: &str, arch: &str, version: &str, status: &str) -> Package {
        Package {
            name: name.into(),
            arch: arch.into(),
            version: version.into(),
            status: Status::parse(status),
        }
    }

    const OK: &str = "install ok installed";

    #[test]
    fn parses_status() {
        let p = parse_database(STATUS.as_bytes(), &[]);
        assert_eq!(
            p,
            vec![
                pkg("bash", "amd64", "5.2.15-2+b2", OK),
                pkg("libc6", "amd64", "2.36-9", OK),
                pkg("libc6", "i386", "2.36-9", OK),
                pkg("old", "all", "1.0", "deinstall ok config-files"),
            ]
        );
    }

    #[test]
    fn applies_journal_in_order() {
        let u1 = b"Package: bash\nStatus: install reinstreq half-installed\nArchitecture: amd64\nVersion: 5.2.15-2+b2\n\nPackage: curl\nStatus: install ok unpacked\nArchitecture: amd64\nVersion: 8.0\n".to_vec();
        let u2 = b"Package: bash\nStatus: hold ok installed\nArchitecture: amd64\nVersion: 5.2.21-2\n\nPackage: libc6\nStatus: purge ok not-installed\nArchitecture: i386\n".to_vec();
        let p = parse_database(STATUS.as_bytes(), &[u1, u2]);
        assert_eq!(
            p,
            vec![
                pkg("bash", "amd64", "5.2.21-2", "hold ok installed"),
                pkg("curl", "amd64", "8.0", "install ok unpacked"),
                pkg("libc6", "amd64", "2.36-9", OK),
                pkg("old", "all", "1.0", "deinstall ok config-files"),
            ]
        );
    }

    #[test]
    fn journal_entry_names() {
        assert!(is_journal_entry(b"0000"));
        assert!(is_journal_entry(b"0123"));
        assert!(!is_journal_entry(b"tmp.i"));
        assert!(!is_journal_entry(b""));
        assert!(!is_journal_entry(b"00a"));
    }

    #[test]
    fn status_note_and_abbrev() {
        let s = |v| Status::parse(v);
        assert_eq!(s(OK).note(), None);
        assert_eq!(s(OK).abbrev(), "ii");
        assert_eq!(s("hold ok installed").note().as_deref(), Some("hold"));
        assert_eq!(s("hold ok installed").abbrev(), "hi");
        assert_eq!(
            s("deinstall ok config-files").note().as_deref(),
            Some("deinstall, config-files")
        );
        assert_eq!(s("deinstall ok config-files").abbrev(), "rc");
        assert_eq!(s("install reinstreq half-installed").abbrev(), "iHR");
        for (status, abbrev) in [
            ("purge ok not-installed", "pn"),
            ("install ok unpacked", "iU"),
            ("install ok half-configured", "iF"),
            ("install ok triggers-awaited", "iW"),
            ("install ok triggers-pending", "it"),
            ("unknown ok not-installed", "un"),
        ] {
            assert_eq!(s(status).abbrev(), abbrev, "{status}");
        }
    }

    #[test]
    fn compares() {
        let a = vec![
            pkg("bash", "amd64", "5.2", OK),
            pkg("curl", "amd64", "8.0", OK),
            pkg("less", "amd64", "6.0", OK),
            pkg("vim", "amd64", "9.0", OK),
        ];
        let b = vec![
            pkg("bash", "amd64", "5.2", OK),
            pkg("curl", "amd64", "8.1", OK),
            pkg("less", "amd64", "6.0", "hold ok installed"),
            pkg("nano", "amd64", "7.0", OK),
        ];
        let d = compare(&a, &b);
        assert_eq!(d.only_a, vec![pkg("vim", "amd64", "9.0", OK)]);
        assert_eq!(d.only_b, vec![pkg("nano", "amd64", "7.0", OK)]);
        let changed: Vec<&str> = d.changed.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(changed, ["curl", "less"]);
        assert_eq!(
            (
                d.changed[0].a.version.as_str(),
                d.changed[0].b.version.as_str()
            ),
            ("8.0", "8.1")
        );
        assert_eq!(d.changed[1].b.status.note().as_deref(), Some("hold"));
        assert!(compare(&a, &a).is_empty());
        for (a, b) in [(&a[..1], &[][..]), (&[][..], &a[..1])] {
            assert!(!compare(a, b).is_empty());
        }
        assert!(!compare(&a[1..2], &b[1..2]).is_empty());
    }
}
