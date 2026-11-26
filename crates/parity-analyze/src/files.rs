//! Comparison of file trees.
//!
//! Collect mode walks each root depth first with the names of a directory in
//! byte order, so entries arrive ordered by path component by component.
//! Both targets get the same roots in the same order, so the two streams can
//! be merged like sorted lists without holding either in memory.

use std::cmp::Ordering;
use std::sync::mpsc::{Receiver, sync_channel};

use parity_proto::{
    Entry, ErrorKind, FileKind, HashState, ReadFileRequest, SensitivePolicy, WalkRequest,
};
use serde::Serialize;

use crate::client::{FileResult, WalkItem};
use crate::error::{Error, Result};
use crate::ssh::Connection;

#[derive(Debug, Clone)]
pub struct FileOptions {
    pub roots: Vec<Vec<u8>>,
    pub excludes: Vec<Vec<u8>>,
    pub max_hash_size: u64,
    pub sensitive: SensitivePolicy,
    pub compare_mtime: bool,
    // Fetch contents of differing text files and show a diff.
    pub content: bool,
    pub max_content_size: u64,
}

#[derive(Debug, Serialize)]
pub struct FileReport {
    pub roots: Vec<String>,
    pub excludes: Vec<String>,
    pub differences: Vec<FileDiff>,
    // Files whose content could not be compared on purpose.
    pub skipped: Vec<Skipped>,
    pub errors: Vec<FileError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    OnlyA,
    OnlyB,
    Both,
}

#[derive(Debug, Serialize)]
pub struct FileDiff {
    pub path: String,
    #[serde(skip)]
    pub raw_path: Vec<u8>,
    pub presence: Presence,
    #[serde(rename = "type")]
    pub kind: &'static str,
    // For a directory present on one side only: number of entries below it,
    // which are not listed one by one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries_below: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<Change>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Change {
    pub field: &'static str,
    pub a: String,
    pub b: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Content {
    Diff { diff: String },
    Binary,
    TooLarge,
    Sensitive,
    Unavailable { reason: String },
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct FileError {
    pub target: String,
    pub path: String,
    pub message: String,
}

impl FileReport {
    pub fn has_differences(&self) -> bool {
        !self.differences.is_empty()
    }
}

pub fn show(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
}

// Orders paths component by component, the order of a depth-first walk with
// sorted names.
pub fn path_cmp(a: &[u8], b: &[u8]) -> Ordering {
    let split = |p: &[u8]| -> Vec<Vec<u8>> {
        p.split(|&c| c == b'/')
            .filter(|c| !c.is_empty())
            .map(<[u8]>::to_vec)
            .collect()
    };
    split(a).cmp(&split(b))
}

pub fn is_within(path: &[u8], prefix: &[u8]) -> bool {
    prefix == b"/"
        || (path.starts_with(prefix) && (path.len() == prefix.len() || path[prefix.len()] == b'/'))
}

// Sorts roots in walk order and drops roots inside other roots, so both
// targets produce one ordered stream.
pub fn normalise_roots(mut roots: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    roots.sort_by(|a, b| path_cmp(a, b));
    let mut out: Vec<Vec<u8>> = Vec::new();
    for r in roots {
        if !out.iter().any(|o| is_within(&r, o)) {
            out.push(r);
        }
    }
    out
}

// Checks a path given on the command line: absolute and normalised, the
// same rules collect mode applies.
pub fn check_path(p: &str) -> Result<Vec<u8>> {
    let ok = p == "/"
        || (p.starts_with('/')
            && p[1..]
                .split('/')
                .all(|c| !c.is_empty() && c != "." && c != ".."));
    if !ok || p.contains('\0') {
        return Err(Error(format!(
            "invalid path '{p}': use an absolute path without '.', '..', '//' or a trailing '/'"
        )));
    }
    Ok(p.as_bytes().to_vec())
}

fn kind_name(k: FileKind) -> &'static str {
    match k {
        FileKind::Regular => "file",
        FileKind::Directory => "directory",
        FileKind::Symlink => "symlink",
        FileKind::CharDevice => "char device",
        FileKind::BlockDevice => "block device",
        FileKind::Fifo => "fifo",
        FileKind::Socket => "socket",
        FileKind::Unknown => "unknown",
    }
}

fn opt<T: std::fmt::Display>(v: &Option<T>) -> String {
    v.as_ref().map_or("-".into(), |v| v.to_string())
}

// Result of the metadata comparison, before contents are fetched.
pub struct Merged {
    pub report: FileReport,
    // Indexes into `report.differences` whose content should be fetched.
    pub content_candidates: Vec<usize>,
}

// Compares two ordered entry streams.
pub fn merge(
    a: impl Iterator<Item = Entry>,
    b: impl Iterator<Item = Entry>,
    opts: &FileOptions,
    targets: [&str; 2],
) -> Merged {
    let mut a = a.peekable();
    let mut b = b.peekable();
    let mut report = FileReport {
        roots: opts.roots.iter().map(|r| show(r)).collect(),
        excludes: opts.excludes.iter().map(|r| show(r)).collect(),
        differences: Vec::new(),
        skipped: Vec::new(),
        errors: Vec::new(),
    };
    let mut candidates = Vec::new();
    // Directory present on one side only, whose children are counted.
    let mut collapse: Option<(Presence, Vec<u8>, usize)> = None;

    loop {
        let order = match (a.peek(), b.peek()) {
            (None, None) => break,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(x), Some(y)) => path_cmp(&x.path, &y.path),
        };
        let (side, e) = match order {
            Ordering::Less => (Presence::OnlyA, a.next().expect("peeked")),
            Ordering::Greater => (Presence::OnlyB, b.next().expect("peeked")),
            Ordering::Equal => {
                let (x, y) = (a.next().expect("peeked"), b.next().expect("peeked"));
                collapse = None;
                compare_pair(&x, &y, opts, targets, &mut report, &mut candidates);
                continue;
            }
        };
        if let Some((s, prefix, idx)) = &collapse
            && *s == side
            && is_within(&e.path, prefix)
        {
            *report.differences[*idx].entries_below.get_or_insert(0) += 1;
            continue;
        }
        let idx = report.differences.len();
        report.differences.push(FileDiff {
            path: show(&e.path),
            raw_path: e.path.clone(),
            presence: side,
            kind: kind_name(e.kind),
            entries_below: None,
            changes: Vec::new(),
            content: None,
        });
        collapse = (e.kind == FileKind::Directory).then_some((side, e.path, idx));
    }
    Merged {
        report,
        content_candidates: candidates,
    }
}

fn compare_pair(
    x: &Entry,
    y: &Entry,
    opts: &FileOptions,
    targets: [&str; 2],
    report: &mut FileReport,
    candidates: &mut Vec<usize>,
) {
    let mut changes = Vec::new();
    let mut push = |field, a: String, b: String| {
        if a != b {
            changes.push(Change { field, a, b });
        }
    };
    if x.kind != y.kind {
        push("type", kind_name(x.kind).into(), kind_name(y.kind).into());
    }
    push("mode", format!("{:04o}", x.mode), format!("{:04o}", y.mode));
    push("uid", x.uid.to_string(), y.uid.to_string());
    push("gid", x.gid.to_string(), y.gid.to_string());
    let same_kind = x.kind == y.kind;
    if same_kind && x.kind == FileKind::Regular && x.size.is_some() && y.size.is_some() {
        push("size", opt(&x.size), opt(&y.size));
    }
    if same_kind
        && x.kind == FileKind::Symlink
        && x.link_target.is_some()
        && y.link_target.is_some()
    {
        let t = |v: &Option<Vec<u8>>| v.as_deref().map(show).unwrap_or_default();
        push("link target", t(&x.link_target), t(&y.link_target));
    }
    if same_kind && x.rdev.is_some() && y.rdev.is_some() {
        push("device", opt(&x.rdev), opt(&y.rdev));
    }
    if opts.compare_mtime && x.mtime.is_some() && y.mtime.is_some() && x.mtime != y.mtime {
        let m =
            |v: &Option<(i64, u32)>| v.map(|(s, ns)| format!("{s}.{ns:09}")).unwrap_or_default();
        push("mtime", m(&x.mtime), m(&y.mtime));
    }

    let mut content_differs = false;
    let mut skip_reason = None;
    if same_kind && x.kind == FileKind::Regular {
        for (e, t) in [(x, targets[0]), (y, targets[1])] {
            if let HashState::Error(m) = &e.hash {
                report.errors.push(FileError {
                    target: t.to_string(),
                    path: show(&e.path),
                    message: format!("cannot hash: {m}"),
                });
            }
        }
        match (&x.hash, &y.hash) {
            (HashState::Xxh3(h1), HashState::Xxh3(h2)) => {
                if h1 != h2 {
                    content_differs = true;
                    changes.push(Change {
                        field: "content",
                        a: hex(&h1[..6]),
                        b: hex(&h2[..6]),
                    });
                }
            }
            (HashState::Withheld, _) | (_, HashState::Withheld) => {
                skip_reason = Some("sensitive path, content not compared")
            }
            (HashState::TooLarge, _) | (_, HashState::TooLarge) => {
                skip_reason = Some("larger than the hash size limit, content not compared")
            }
            (HashState::PseudoFs, _) | (_, HashState::PseudoFs) => {
                skip_reason =
                    Some("on a pseudo file system (proc, sysfs, ...), content not compared")
            }
            _ => {}
        }
    }
    let size_differs = changes.iter().any(|c| c.field == "size");
    if let Some(reason) = skip_reason
        && !size_differs
    {
        report.skipped.push(Skipped {
            path: show(&x.path),
            reason: reason.into(),
        });
    }
    if changes.is_empty() {
        return;
    }
    let idx = report.differences.len();
    let sensitive = x.sensitive || y.sensitive;
    let fits = |e: &Entry| e.size.is_some_and(|s| s <= opts.max_content_size);
    let content = if !content_differs || !opts.content {
        None
    } else if sensitive {
        Some(Content::Sensitive)
    } else if !(fits(x) && fits(y)) {
        Some(Content::TooLarge)
    } else {
        candidates.push(idx);
        None
    };
    report.differences.push(FileDiff {
        path: show(&x.path),
        raw_path: x.path.clone(),
        presence: Presence::Both,
        kind: kind_name(x.kind),
        entries_below: None,
        changes,
        content,
    });
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn is_text(b: &[u8]) -> bool {
    !b.contains(&0) && std::str::from_utf8(b).is_ok()
}

// A unified diff for text, otherwise a note that the files are binary.
pub fn content_diff(labels: [&str; 2], a: &[u8], b: &[u8]) -> Content {
    if !is_text(a) || !is_text(b) {
        return Content::Binary;
    }
    let (a, b) = (
        std::str::from_utf8(a).expect("checked"),
        std::str::from_utf8(b).expect("checked"),
    );
    let diff = similar::TextDiff::from_lines(a, b)
        .unified_diff()
        .context_radius(3)
        .header(labels[0], labels[1])
        .to_string();
    Content::Diff { diff }
}

struct Stream {
    rx: Receiver<WalkItem>,
    buf: std::vec::IntoIter<Entry>,
    target: String,
    errors: Vec<FileError>,
}

impl Iterator for Stream {
    type Item = Entry;
    fn next(&mut self) -> Option<Entry> {
        loop {
            if let Some(e) = self.buf.next() {
                return Some(e);
            }
            match self.rx.recv() {
                Ok(WalkItem::Entries(v)) => self.buf = v.into_iter(),
                Ok(WalkItem::Error { path, message }) => self.errors.push(FileError {
                    target: self.target.clone(),
                    path: show(&path),
                    message,
                }),
                Err(_) => return None,
            }
        }
    }
}

// Walks both targets in parallel and compares, then fetches the contents of
// differing text files.
pub fn compare(conns: [&mut Connection; 2], opts: &FileOptions) -> Result<FileReport> {
    let req = WalkRequest {
        roots: opts.roots.clone(),
        excludes: opts.excludes.clone(),
        hash: true,
        max_hash_size: opts.max_hash_size,
        sensitive: opts.sensitive.clone(),
        batch_size: 0,
    };
    let targets = [conns[0].target.clone(), conns[1].target.clone()];
    let [ca, cb] = conns;
    let mut merged = std::thread::scope(|s| -> Result<Merged> {
        let mut streams = Vec::new();
        let mut handles = Vec::new();
        for (c, t) in [ca as &mut Connection, cb].into_iter().zip(&targets) {
            let (tx, rx) = sync_channel(16);
            let req = req.clone();
            handles.push(s.spawn(move || {
                c.walk(req, |item| {
                    // The receiver only goes away on a panic; keep reading so the
                    // session stays in step.
                    let _ = tx.send(item);
                })
            }));
            streams.push(Stream {
                rx,
                buf: Vec::new().into_iter(),
                target: t.clone(),
                errors: Vec::new(),
            });
        }
        let [mut sa, mut sb]: [Stream; 2] = streams.try_into().ok().expect("two streams");
        let mut m = merge(&mut sa, &mut sb, opts, [&targets[0], &targets[1]]);
        for h in handles {
            h.join().expect("walk thread panicked")?;
        }
        m.report.errors.extend(sa.errors);
        m.report.errors.extend(sb.errors);
        Ok(m)
    })?;
    // The connections are free again after the scope.
    fetch_contents(&mut merged, [ca, cb], opts, &targets)?;
    Ok(merged.report)
}

fn fetch_contents(
    m: &mut Merged,
    conns: [&mut Connection; 2],
    opts: &FileOptions,
    targets: &[String; 2],
) -> Result<()> {
    let [ca, cb] = conns;
    let candidates = std::mem::take(&mut m.content_candidates);
    for group in candidates.chunks(64) {
        let paths: Vec<Vec<u8>> = group
            .iter()
            .map(|&i| m.report.differences[i].raw_path.clone())
            .collect();
        let fetch = |c: &mut Connection| -> Result<Vec<FileResult>> {
            paths
                .iter()
                .map(|p| {
                    c.read_file(ReadFileRequest {
                        path: p.clone(),
                        max_size: opts.max_content_size,
                        sensitive: opts.sensitive.clone(),
                    })
                })
                .collect()
        };
        let (ra, rb) = std::thread::scope(|s| {
            let ha = s.spawn(|| fetch(ca));
            let rb = fetch(cb);
            (ha.join().expect("fetch thread panicked"), rb)
        });
        let (ra, rb) = (ra?, rb?);
        for ((&i, a), b) in group.iter().zip(ra).zip(rb) {
            let d = &mut m.report.differences[i];
            d.content = Some(match (a, b) {
                (Ok(a), Ok(b)) => content_diff(
                    [
                        &format!("{}:{}", targets[0], d.path),
                        &format!("{}:{}", targets[1], d.path),
                    ],
                    &a,
                    &b,
                ),
                (Err((ErrorKind::Sensitive, _)), _) | (_, Err((ErrorKind::Sensitive, _))) => {
                    Content::Sensitive
                }
                (Err((ErrorKind::TooLarge, _)), _) | (_, Err((ErrorKind::TooLarge, _))) => {
                    Content::TooLarge
                }
                (Err((_, m)), _) | (_, Err((_, m))) => Content::Unavailable { reason: m },
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> FileOptions {
        FileOptions {
            roots: vec![b"/etc".to_vec()],
            excludes: vec![],
            max_hash_size: 1 << 20,
            sensitive: SensitivePolicy::default(),
            compare_mtime: false,
            content: true,
            max_content_size: 1 << 20,
        }
    }

    fn e(path: &str, kind: FileKind) -> Entry {
        Entry {
            path: path.as_bytes().to_vec(),
            kind,
            mode: 0o644,
            uid: 0,
            gid: 0,
            size: Some(1),
            mtime: Some((1, 0)),
            link_target: None,
            rdev: None,
            hash: if kind == FileKind::Regular {
                HashState::Xxh3([1; 16])
            } else {
                HashState::None
            },
            sensitive: false,
        }
    }

    fn f(path: &str) -> Entry {
        e(path, FileKind::Regular)
    }
    fn d(path: &str) -> Entry {
        e(path, FileKind::Directory)
    }

    fn run(a: Vec<Entry>, b: Vec<Entry>, o: &FileOptions) -> Merged {
        merge(a.into_iter(), b.into_iter(), o, ["a", "b"])
    }

    #[test]
    fn ordering_matches_walk_order() {
        let mut v = vec!["/etc/a-b", "/etc/a/b", "/etc/a", "/etc", "/etc/ab", "/"];
        v.sort_by(|a, b| path_cmp(a.as_bytes(), b.as_bytes()));
        assert_eq!(
            v,
            ["/", "/etc", "/etc/a", "/etc/a/b", "/etc/a-b", "/etc/ab"]
        );
    }

    #[test]
    fn roots_are_normalised() {
        let r = normalise_roots(vec![
            b"/usr/local/etc".to_vec(),
            b"/etc/ssh".to_vec(),
            b"/etc".to_vec(),
        ]);
        assert_eq!(r, vec![b"/etc".to_vec(), b"/usr/local/etc".to_vec()]);
        assert_eq!(
            normalise_roots(vec![b"/etc".to_vec(), b"/".to_vec()]),
            vec![b"/".to_vec()]
        );
    }

    #[test]
    fn path_checks() {
        assert!(check_path("/etc").is_ok());
        assert!(check_path("/").is_ok());
        for p in ["etc", "/etc/", "/etc/../x", "//etc", "/etc/."] {
            assert!(check_path(p).is_err(), "{p}");
        }
    }

    #[test]
    fn identical_trees() {
        let t = || vec![d("/etc"), f("/etc/a"), f("/etc/b")];
        let m = run(t(), t(), &opts());
        assert!(!m.report.has_differences());
        assert!(m.content_candidates.is_empty());
    }

    #[test]
    fn presence_and_collapsing() {
        let a = vec![
            d("/etc"),
            d("/etc/only"),
            f("/etc/only/x"),
            d("/etc/only/sub"),
            f("/etc/only/sub/y"),
            f("/etc/z"),
        ];
        let b = vec![d("/etc"), f("/etc/new"), f("/etc/z")];
        let m = run(a, b, &opts());
        let diffs: Vec<_> = m
            .report
            .differences
            .iter()
            .map(|d| (d.path.as_str(), d.presence, d.entries_below))
            .collect();
        assert_eq!(
            diffs,
            vec![
                ("/etc/new", Presence::OnlyB, None),
                ("/etc/only", Presence::OnlyA, Some(3)),
            ]
        );
    }

    #[test]
    fn metadata_changes() {
        let mut x = f("/etc/a");
        x.mode = 0o600;
        x.uid = 5;
        x.size = Some(2);
        x.hash = HashState::Xxh3([2; 16]);
        x.mtime = Some((2, 0));
        let m = run(vec![x], vec![f("/etc/a")], &opts());
        let fields: Vec<_> = m.report.differences[0]
            .changes
            .iter()
            .map(|c| c.field)
            .collect();
        assert_eq!(fields, ["mode", "uid", "size", "content"]);
        assert_eq!(m.content_candidates, vec![0]);

        let mut o = opts();
        o.compare_mtime = true;
        let mut x = f("/etc/a");
        x.mtime = Some((2, 0));
        let m = run(vec![x], vec![f("/etc/a")], &o);
        assert_eq!(m.report.differences[0].changes[0].field, "mtime");
    }

    #[test]
    fn type_change_and_links() {
        let mut l = e("/etc/a", FileKind::Symlink);
        l.link_target = Some(b"x".to_vec());
        let mut l2 = l.clone();
        l2.link_target = Some(b"y".to_vec());
        let m = run(vec![l.clone()], vec![l2], &opts());
        assert_eq!(m.report.differences[0].changes[0].field, "link target");
        let m = run(vec![l], vec![f("/etc/a")], &opts());
        assert_eq!(m.report.differences[0].changes[0].field, "type");
    }

    #[test]
    fn skipped_files_are_listed_not_different() {
        let mut big = f("/etc/big");
        big.hash = HashState::TooLarge;
        let mut s = f("/etc/shadow");
        s.hash = HashState::Withheld;
        s.size = None;
        s.sensitive = true;
        let m = run(vec![big.clone(), s.clone()], vec![big, s], &opts());
        assert!(!m.report.has_differences());
        assert_eq!(m.report.skipped.len(), 2);
    }

    #[test]
    fn sensitive_and_large_content_is_not_fetched() {
        let mut x = f("/etc/s");
        x.sensitive = true;
        x.hash = HashState::Xxh3([3; 16]);
        let mut y = f("/etc/s");
        y.sensitive = true;
        let mut big = f("/etc/t");
        big.size = Some(10 << 20);
        big.hash = HashState::Xxh3([3; 16]);
        let mut big2 = big.clone();
        big2.hash = HashState::Xxh3([4; 16]);
        let m = run(vec![x, big], vec![y, big2], &opts());
        assert!(m.content_candidates.is_empty());
        assert_eq!(m.report.differences[0].content, Some(Content::Sensitive));
        assert_eq!(m.report.differences[1].content, Some(Content::TooLarge));
    }

    #[test]
    fn hash_errors_are_errors() {
        let mut x = f("/etc/a");
        x.hash = HashState::Error("I/O error".into());
        let m = run(vec![x], vec![f("/etc/a")], &opts());
        assert_eq!(m.report.errors.len(), 1);
        assert_eq!(m.report.errors[0].target, "a");
    }

    #[test]
    fn diffs() {
        let c = content_diff(["a:/f", "b:/f"], b"one\ntwo\n", b"one\nthree\n");
        let Content::Diff { diff } = c else { panic!() };
        assert!(
            diff.contains("--- a:/f") && diff.contains("-two") && diff.contains("+three"),
            "{diff}"
        );
        assert_eq!(content_diff(["a", "b"], b"\0\x01", b"x"), Content::Binary);
        assert_eq!(content_diff(["a", "b"], b"\xff", b"x"), Content::Binary);
    }
}
