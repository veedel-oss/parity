//! Output: a readable report for people and JSON for scripts.

use std::fmt::Write as _;

use serde::Serialize;

use crate::files::{Content, FileReport, Presence};
use crate::packages::{Package, PackageDiff};

#[derive(Debug, Serialize)]
pub struct Report {
    pub targets: [String; 2],
    // `identical`, `different` or `error` (see the exit codes).
    pub result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packages: Option<PackageDiff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<FileReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

pub const EXIT_IDENTICAL: i32 = 0;
pub const EXIT_DIFFERENT: i32 = 1;
pub const EXIT_ERROR: i32 = 2;

impl Report {
    pub fn new(
        targets: [String; 2],
        packages: Option<PackageDiff>,
        files: Option<FileReport>,
        notes: Vec<String>,
    ) -> Self {
        let mut r = Report {
            targets,
            result: "identical",
            packages,
            files,
            notes,
        };
        r.result = match r.exit_code() {
            EXIT_IDENTICAL => "identical",
            EXIT_DIFFERENT => "different",
            _ => "error",
        };
        r
    }

    // 0 identical, 1 different, 2 if the comparison was incomplete. Files
    // skipped on purpose do not count.
    pub fn exit_code(&self) -> i32 {
        if self.files.as_ref().is_some_and(|f| !f.errors.is_empty()) {
            return EXIT_ERROR;
        }
        let pkgs = self.packages.as_ref().is_some_and(|p| !p.is_empty());
        let files = self.files.as_ref().is_some_and(|f| f.has_differences());
        if pkgs || files {
            EXIT_DIFFERENT
        } else {
            EXIT_IDENTICAL
        }
    }

    pub fn json(&self) -> String {
        serde_json::to_string_pretty(self).expect("report serialises") + "\n"
    }

    pub fn text(&self) -> String {
        let [a, b] = &self.targets;
        let mut o = String::new();
        let _ = writeln!(o, "Comparing {a} (a) with {b} (b)");
        for n in &self.notes {
            let _ = writeln!(o, "Note: {n}");
        }
        if let Some(p) = &self.packages {
            text_packages(&mut o, p, a, b);
        }
        if let Some(f) = &self.files {
            text_files(&mut o, f, a, b);
        }
        let _ = writeln!(
            o,
            "\nResult: {}",
            match self.result {
                "identical" => "identical",
                "different" => "differences found",
                _ => "comparison incomplete, see the errors above",
            }
        );
        o
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn table(o: &mut String, rows: &[Vec<String>]) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for r in rows {
        let mut line = String::from("  ");
        for (i, cell) in r.iter().enumerate() {
            if i + 1 == r.len() {
                line.push_str(cell);
            } else {
                let _ = write!(line, "{cell:<w$}  ", w = widths[i]);
            }
        }
        let _ = writeln!(o, "{}", line.trim_end());
    }
}

fn text_packages(o: &mut String, p: &PackageDiff, a: &str, b: &str) {
    let n = p.only_a.len() + p.only_b.len() + p.changed.len();
    let _ = writeln!(
        o,
        "\nPackages: {}",
        if n == 0 {
            "identical".into()
        } else {
            plural(n, "difference", "differences")
        }
    );
    if n == 0 {
        return;
    }
    let mut rows = vec![vec![
        "PACKAGE".into(),
        "ARCH".into(),
        format!("{a} (a)"),
        format!("{b} (b)"),
    ]];
    let show = |p: &Package| {
        if p.state == "installed" {
            p.version.clone()
        } else {
            format!("{} ({})", p.version, p.state)
        }
    };
    for x in &p.only_a {
        rows.push(vec![x.name.clone(), x.arch.clone(), show(x), "-".into()]);
    }
    for x in &p.only_b {
        rows.push(vec![x.name.clone(), x.arch.clone(), "-".into(), show(x)]);
    }
    for c in &p.changed {
        let s = |v: &crate::packages::Side| {
            if v.state == "installed" {
                v.version.clone()
            } else {
                format!("{} ({})", v.version, v.state)
            }
        };
        rows.push(vec![c.name.clone(), c.arch.clone(), s(&c.a), s(&c.b)]);
    }
    rows[1..].sort();
    table(o, &rows);
}

fn parent(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) => "/",
        Some(i) => &p[..i],
        None => p,
    }
}

fn text_files(o: &mut String, f: &FileReport, a: &str, b: &str) {
    let n = f.differences.len();
    let _ = writeln!(
        o,
        "\nFiles below {}: {}",
        f.roots.join(", "),
        if n == 0 {
            "identical".into()
        } else {
            plural(n, "difference", "differences")
        }
    );
    let mut dir = None;
    for d in &f.differences {
        let p = parent(&d.path);
        if dir != Some(p) {
            let _ = writeln!(o, "{p}");
            dir = Some(p);
        }
        let name = if d.path == "/" {
            "/"
        } else {
            &d.path[p.len() + usize::from(p != "/")..]
        };
        let what = match d.presence {
            Presence::OnlyA => format!("only on {a}"),
            Presence::OnlyB => format!("only on {b}"),
            Presence::Both => "changed".into(),
        };
        let mut line = format!(
            "  {what:<w$}  {:<9}  {name}",
            d.kind,
            w = a.len().max(b.len()) + 8
        );
        if let Some(k) = d.entries_below {
            let _ = write!(
                line,
                " (and {} below)",
                plural(k as usize, "entry", "entries")
            );
        }
        let _ = writeln!(o, "{}", line.trim_end());
        for c in &d.changes {
            let _ = writeln!(o, "      {}: {} -> {}", c.field, c.a, c.b);
        }
        match &d.content {
            Some(Content::Diff { diff }) => {
                for l in diff.lines() {
                    let _ = writeln!(o, "      {l}");
                }
            }
            Some(Content::Binary) => {
                let _ = writeln!(o, "      binary files differ");
            }
            Some(Content::TooLarge) => {
                let _ = writeln!(o, "      content differs, too large for a diff");
            }
            Some(Content::Sensitive) => {
                let _ = writeln!(o, "      content differs, sensitive path: no diff");
            }
            Some(Content::Unavailable { reason }) => {
                let _ = writeln!(o, "      content differs, no diff: {reason}");
            }
            None => {}
        }
    }
    if !f.skipped.is_empty() {
        let _ = writeln!(o, "\nSkipped on purpose ({}):", f.skipped.len());
        for s in &f.skipped {
            let _ = writeln!(o, "  {}: {}", s.path, s.reason);
        }
    }
    if !f.excludes.is_empty() {
        let _ = writeln!(o, "\nExcluded: {}", f.excludes.join(", "));
    }
    if !f.errors.is_empty() {
        let _ = writeln!(o, "\nErrors ({}):", f.errors.len());
        for e in &f.errors {
            let _ = writeln!(o, "  [{}] {}: {}", e.target, e.path, e.message);
        }
    }
}

// Package list of one target.
pub fn packages_text(list: &[Package]) -> String {
    let mut rows = vec![vec![
        "PACKAGE".into(),
        "ARCH".into(),
        "VERSION".into(),
        "STATE".into(),
    ]];
    rows.extend(list.iter().map(|p| {
        vec![
            p.name.clone(),
            p.arch.clone(),
            p.version.clone(),
            p.state.clone(),
        ]
    }));
    let mut o = String::new();
    table(&mut o, &rows);
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::{Change, FileDiff, FileError, Skipped};

    fn files(differences: Vec<FileDiff>, errors: Vec<FileError>) -> FileReport {
        FileReport {
            roots: vec!["/etc".into()],
            excludes: vec![],
            differences,
            skipped: vec![Skipped {
                path: "/etc/shadow".into(),
                reason: "sensitive".into(),
            }],
            errors,
        }
    }

    fn diff(path: &str) -> FileDiff {
        FileDiff {
            path: path.into(),
            raw_path: path.as_bytes().to_vec(),
            presence: Presence::Both,
            kind: "file",
            entries_below: None,
            changes: vec![Change {
                field: "mode",
                a: "0644".into(),
                b: "0600".into(),
            }],
            content: Some(Content::Diff {
                diff: "--- a\n+++ b\n-x\n+y\n".into(),
            }),
        }
    }

    fn t() -> [String; 2] {
        ["host-a".into(), "host-b".into()]
    }

    #[test]
    fn exit_codes() {
        let r = Report::new(
            t(),
            Some(PackageDiff::default()),
            Some(files(vec![], vec![])),
            vec![],
        );
        assert_eq!(
            (r.exit_code(), r.result),
            (0, "identical"),
            "skipped files do not count"
        );
        let r = Report::new(t(), None, Some(files(vec![diff("/etc/a")], vec![])), vec![]);
        assert_eq!((r.exit_code(), r.result), (1, "different"));
        let err = FileError {
            target: "host-a".into(),
            path: "/etc/x".into(),
            message: "I/O error".into(),
        };
        let r = Report::new(
            t(),
            None,
            Some(files(vec![diff("/etc/a")], vec![err])),
            vec![],
        );
        assert_eq!((r.exit_code(), r.result), (2, "error"));
    }

    #[test]
    fn text_groups_by_directory() {
        let r = Report::new(
            t(),
            None,
            Some(files(
                vec![diff("/etc/a"), diff("/etc/b"), diff("/etc/ssh/c")],
                vec![],
            )),
            vec![],
        );
        let s = r.text();
        assert_eq!(s.matches("\n/etc\n").count(), 1, "{s}");
        assert!(s.contains("\n/etc/ssh\n"), "{s}");
        assert!(s.contains("mode: 0644 -> 0600"));
        assert!(s.contains("+y"));
        assert!(s.contains("Skipped on purpose"));
    }

    #[test]
    fn json_is_valid() {
        let r = Report::new(
            t(),
            Some(PackageDiff::default()),
            Some(files(vec![diff("/etc/a")], vec![])),
            vec![],
        );
        let v: serde_json::Value = serde_json::from_str(&r.json()).unwrap();
        assert_eq!(v["result"], "different");
        assert_eq!(v["files"]["differences"][0]["path"], "/etc/a");
        assert_eq!(v["files"]["differences"][0]["content"]["status"], "diff");
        assert!(v["files"]["differences"][0].get("raw_path").is_none());
    }
}
