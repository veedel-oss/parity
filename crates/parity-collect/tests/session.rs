//! Collect mode over byte streams. The root check is bypassed by calling the
//! session directly; everything else is the real code path.
//!
//! The tests never depend on the system that runs them: files they read are
//! fixtures they create, and the paths collect mode reads on its own (the
//! dpkg database, the built-in sensitive list) are replaced by fixtures
//! through [`session::Config`]. What only a real target can show (access
//! times on its file systems, `/proc`) is tested in `tests/vm`.

use std::io::{self, Cursor, Write};
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use parity_collect::session::{self, Config, End};
use parity_proto::*;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("parity-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        // Canonical, so no symlink (for example /tmp on some systems) is in
        // the path.
        TempDir(p.canonicalize().unwrap())
    }
    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
    fn bytes(&self, rel: &str) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        let p = if rel.is_empty() {
            self.0.clone()
        } else {
            self.path(rel)
        };
        p.as_os_str().as_bytes().to_vec()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn frames(hello: Option<Hello>, reqs: &[Request]) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(h) = hello {
        write_frame(&mut buf, &h.encode()).unwrap();
    }
    for r in reqs {
        write_frame(&mut buf, &r.encode()).unwrap();
    }
    buf
}

// No built-in sensitive patterns and a dpkg database that does not exist:
// nothing outside the fixtures is read. Tests of these set their own.
fn config() -> Config {
    Config {
        dpkg_status: b"/nonexistent/parity-test/dpkg-status".to_vec(),
        sensitive: Vec::new(),
    }
}

// Runs a session and returns its result, the collect hello and the responses.
fn session(reqs: &[Request]) -> (Result<End, String>, Hello, Vec<Response>) {
    session_in(&config(), Some(Hello::current()), reqs)
}

fn session_with(
    hello: Option<Hello>,
    reqs: &[Request],
) -> (Result<End, String>, Hello, Vec<Response>) {
    session_in(&config(), hello, reqs)
}

fn session_in(
    config: &Config,
    hello: Option<Hello>,
    reqs: &[Request],
) -> (Result<End, String>, Hello, Vec<Response>) {
    let mut input = Cursor::new(frames(hello, reqs));
    let mut output = Vec::new();
    let r = session::run(&mut input, &mut output, config);
    let mut out = &output[..];
    let h = Hello::decode(&read_frame(&mut out, MAX_FRAME).unwrap().unwrap()).unwrap();
    let mut resp = Vec::new();
    while let Some(f) = read_frame(&mut out, MAX_FRAME).unwrap() {
        resp.push(Response::decode(&f).unwrap());
    }
    (r, h, resp)
}

#[test]
fn ping_and_end() {
    let (r, h, resp) = session(&[
        Request::Ping { nonce: 7 },
        Request::End,
        Request::Ping { nonce: 8 },
    ]);
    assert_eq!(r, Ok(End::Request));
    assert_eq!(h, Hello::current());
    assert_eq!(resp, vec![Response::Pong { nonce: 7 }, Response::Bye]);
}

#[test]
fn end_of_input_ends_session() {
    let (r, _, resp) = session(&[Request::Ping { nonce: 1 }]);
    assert_eq!(r, Ok(End::Eof));
    assert_eq!(resp, vec![Response::Pong { nonce: 1 }]);
    let (r, _, resp) = session_with(None, &[]);
    assert_eq!(r, Ok(End::Eof));
    assert!(resp.is_empty());
}

#[test]
fn version_mismatch() {
    let h = Hello {
        protocol: PROTOCOL_VERSION + 1,
        version: "9.9.9".into(),
    };
    let (r, _, resp) = session_with(Some(h), &[Request::Ping { nonce: 1 }]);
    let e = r.unwrap_err();
    assert!(
        e.contains("9.9.9") && e.contains(env!("CARGO_PKG_VERSION")),
        "{e}"
    );
    assert!(resp.is_empty());
}

#[test]
fn invalid_request_ends_session() {
    let mut buf = frames(Some(Hello::current()), &[]);
    write_frame(&mut buf, &[200]).unwrap();
    write_frame(&mut buf, &Request::Ping { nonce: 1 }.encode()).unwrap();
    let mut output = Vec::new();
    assert!(session::run(&mut Cursor::new(buf), &mut output, &config()).is_err());
    let mut out = &output[..];
    read_frame(&mut out, MAX_FRAME).unwrap();
    let r = Response::decode(&read_frame(&mut out, MAX_FRAME).unwrap().unwrap()).unwrap();
    assert!(matches!(
        r,
        Response::Error {
            kind: ErrorKind::InvalidRequest,
            ..
        }
    ));
    assert!(read_frame(&mut out, MAX_FRAME).unwrap().is_none());
}

#[test]
fn oversized_request_is_rejected() {
    let mut buf = frames(Some(Hello::current()), &[]);
    buf.extend_from_slice(&((MAX_REQUEST_FRAME as u32) + 1).to_be_bytes());
    let mut output = Vec::new();
    assert!(session::run(&mut Cursor::new(buf), &mut output, &config()).is_err());
}

struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn write_error_ends_session() {
    let mut input = Cursor::new(frames(
        Some(Hello::current()),
        &[Request::Ping { nonce: 1 }],
    ));
    assert!(session::run(&mut input, &mut Broken, &config()).is_err());
}

type WalkErrors = Vec<(Vec<u8>, String)>;

fn walk(req: WalkRequest) -> (Vec<Entry>, WalkErrors, usize) {
    let (r, _, resp) = session(&[Request::Walk(req), Request::End]);
    assert_eq!(r, Ok(End::Request));
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    let mut batches = 0;
    let mut ended = false;
    for x in resp {
        match x {
            Response::Entries(v) => {
                assert!(!ended);
                batches += 1;
                entries.extend(v);
            }
            Response::WalkError { path, message } => errors.push((path, message)),
            Response::WalkEnd => ended = true,
            Response::Bye => assert!(ended),
            other => panic!("unexpected {other:?}"),
        }
    }
    (entries, errors, batches)
}

fn find<'a>(entries: &'a [Entry], p: &[u8]) -> &'a Entry {
    entries
        .iter()
        .find(|e| e.path == p)
        .unwrap_or_else(|| panic!("{} missing", String::from_utf8_lossy(p)))
}

fn fixture(name: &str) -> TempDir {
    let t = TempDir::new(name);
    std::fs::create_dir(t.path("sub")).unwrap();
    std::fs::write(t.path("sub/a.conf"), b"abc").unwrap();
    std::fs::write(t.path("big"), vec![0u8; 5000]).unwrap();
    std::fs::create_dir(t.path("skip")).unwrap();
    std::fs::write(t.path("skip/x"), b"x").unwrap();
    std::fs::create_dir(t.path("secret")).unwrap();
    std::fs::write(t.path("secret/key"), b"hunter2").unwrap();
    symlink("sub", t.path("link")).unwrap();
    symlink("/nonexistent", t.path("dangling")).unwrap();
    t
}

#[test]
fn walk_reports_metadata_hashes_and_links() {
    let t = fixture("walk");
    let (entries, errors, _) = walk(WalkRequest {
        roots: vec![t.bytes("")],
        excludes: vec![t.bytes("skip")],
        hash: true,
        max_hash_size: 1000,
        sensitive: SensitivePolicy {
            extra: vec![t.bytes("secret")],
            metadata: false,
        },
        batch_size: 2,
    });
    assert!(errors.is_empty(), "{errors:?}");

    let root = find(&entries, &t.bytes(""));
    assert_eq!(root.kind, FileKind::Directory);
    assert_eq!(entries[0].path, t.bytes(""), "pre-order, root first");

    let a = find(&entries, &t.bytes("sub/a.conf"));
    assert_eq!(a.kind, FileKind::Regular);
    assert_eq!(a.size, Some(3));
    let HashState::Sha256(h) = a.hash else {
        panic!("{:?}", a.hash)
    };
    assert_eq!(h[..4], [0xba, 0x78, 0x16, 0xbf], "sha256 of abc");
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        a.uid,
        std::fs::metadata(t.path("sub/a.conf")).unwrap().uid()
    );
    assert!(a.mtime.is_some());

    assert_eq!(find(&entries, &t.bytes("big")).hash, HashState::TooLarge);

    let link = find(&entries, &t.bytes("link"));
    assert_eq!(link.kind, FileKind::Symlink);
    assert_eq!(link.link_target.as_deref(), Some(&b"sub"[..]));
    assert!(
        !entries
            .iter()
            .any(|e| e.path.starts_with(&t.bytes("link/"))),
        "symlink not followed"
    );
    assert_eq!(
        find(&entries, &t.bytes("dangling")).link_target.as_deref(),
        Some(&b"/nonexistent"[..])
    );

    assert!(
        !entries.iter().any(|e| e.path.starts_with(&t.bytes("skip"))),
        "excluded"
    );

    for p in ["secret", "secret/key"] {
        let s = find(&entries, &t.bytes(p));
        assert!(s.sensitive);
        assert_eq!((s.size, s.mtime, &s.link_target), (None, None, &None));
    }
    assert_eq!(
        find(&entries, &t.bytes("secret/key")).hash,
        HashState::Withheld
    );
}

#[test]
fn walk_streams_in_batches() {
    let t = TempDir::new("batches");
    for i in 0..25 {
        std::fs::write(t.path(&format!("f{i:02}")), b"").unwrap();
    }
    let (entries, _, batches) = walk(WalkRequest {
        roots: vec![t.bytes("")],
        batch_size: 10,
        ..Default::default()
    });
    assert_eq!(entries.len(), 26);
    assert_eq!(batches, 3);
    let mut sorted = entries.clone();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    assert_eq!(entries, sorted, "names are sorted");
}

#[test]
fn walk_sensitive_metadata_opt_in() {
    let t = fixture("optin");
    let (entries, _, _) = walk(WalkRequest {
        roots: vec![t.bytes("secret")],
        hash: true,
        max_hash_size: 1000,
        sensitive: SensitivePolicy {
            extra: vec![t.bytes("secret")],
            metadata: true,
        },
        ..Default::default()
    });
    let k = find(&entries, &t.bytes("secret/key"));
    assert!(k.sensitive);
    assert_eq!(k.size, Some(7));
    assert!(matches!(k.hash, HashState::Sha256(_)));
}

#[test]
fn walk_root_through_symlink_is_refused() {
    let t = fixture("rootlink");
    let (entries, errors, _) = walk(WalkRequest {
        roots: vec![t.bytes("link/a.conf"), t.bytes("missing")],
        ..Default::default()
    });
    assert!(entries.is_empty());
    assert_eq!(
        errors.len(),
        1,
        "missing root is silent, symlink is an error"
    );
}

#[test]
fn walk_rejects_invalid_requests() {
    for roots in [
        vec![b"relative".to_vec()],
        vec![b"/a/../etc".to_vec()],
        vec![b"/x".to_vec(); 65],
    ] {
        let (_, _, resp) = session(&[Request::Walk(WalkRequest {
            roots,
            ..Default::default()
        })]);
        assert!(matches!(
            resp[..],
            [Response::Error {
                kind: ErrorKind::InvalidRequest,
                ..
            }]
        ));
    }
}

fn read(p: Vec<u8>, max: u64, extra: Vec<Vec<u8>>, metadata: bool) -> Result<Vec<u8>, ErrorKind> {
    read_in(&config(), p, max, extra, metadata)
}

fn read_in(
    config: &Config,
    p: Vec<u8>,
    max: u64,
    extra: Vec<Vec<u8>>,
    metadata: bool,
) -> Result<Vec<u8>, ErrorKind> {
    let (_, _, resp) = session_in(
        config,
        Some(Hello::current()),
        &[Request::ReadFile(ReadFileRequest {
            path: p,
            max_size: max,
            sensitive: SensitivePolicy { extra, metadata },
        })],
    );
    let mut data = Vec::new();
    for r in resp {
        match r {
            Response::Chunk(b) => data.extend(b),
            Response::ChunkEnd => return Ok(data),
            Response::Error { kind, .. } => return Err(kind),
            other => panic!("unexpected {other:?}"),
        }
    }
    panic!("no end");
}

#[test]
fn read_file() {
    let t = fixture("read");
    assert_eq!(
        read(t.bytes("sub/a.conf"), 100, vec![], false),
        Ok(b"abc".to_vec())
    );
    assert_eq!(
        read(t.bytes("big"), 100, vec![], false),
        Err(ErrorKind::TooLarge)
    );
    assert_eq!(
        read(t.bytes("nope"), 100, vec![], false),
        Err(ErrorKind::NotFound)
    );
    assert_eq!(
        read(t.bytes("sub"), 100, vec![], false),
        Err(ErrorKind::NotRegular)
    );
}

#[test]
fn read_file_never_follows_symlinks() {
    let t = fixture("readlink");
    std::fs::write(t.path("target"), b"t").unwrap();
    symlink(t.path("target"), t.path("filelink")).unwrap();
    assert_eq!(
        read(t.bytes("filelink"), 100, vec![], false),
        Err(ErrorKind::NotRegular)
    );
    assert_eq!(
        read(t.bytes("link/a.conf"), 100, vec![], false),
        Err(ErrorKind::NotRegular)
    );
}

#[test]
fn read_file_large_content_is_chunked() {
    let t = TempDir::new("chunks");
    let data: Vec<u8> = (0..(CHUNK_SIZE * 2 + 17)).map(|i| i as u8).collect();
    std::fs::write(t.path("f"), &data).unwrap();
    assert_eq!(read(t.bytes("f"), u64::MAX, vec![], false), Ok(data));
}

#[test]
fn sensitive_content_is_never_sent() {
    let t = fixture("sensitive");
    let extra = vec![t.bytes("secret")];
    for metadata in [false, true] {
        assert_eq!(
            read(t.bytes("secret/key"), 100, extra.clone(), metadata),
            Err(ErrorKind::Sensitive)
        );
    }
    // Built-in patterns apply whatever the request says (no extra patterns,
    // metadata opt-in). Which paths the real list holds is a unit test in
    // sensitive.rs.
    let builtin = Config {
        sensitive: vec![t.bytes("secret"), t.bytes("sub/*.conf")],
        ..config()
    };
    for p in ["secret/key", "sub/a.conf"] {
        assert_eq!(
            read_in(&builtin, t.bytes(p), u64::MAX, vec![], true),
            Err(ErrorKind::Sensitive),
            "{p}"
        );
    }
    // Ways around the path check are rejected before any file is opened.
    for p in [
        &b"/etc/../etc/shadow"[..],
        b"/etc//shadow",
        b"/etc/./shadow",
        b"etc/shadow",
        b"/etc/shadow/",
    ] {
        assert_eq!(
            read(p.to_vec(), u64::MAX, vec![], true),
            Err(ErrorKind::InvalidRequest)
        );
    }
    // A symlink to a sensitive file is not followed.
    symlink(t.path("secret/key"), t.path("alias")).unwrap();
    assert_eq!(
        read(t.bytes("alias"), 100, extra, true),
        Err(ErrorKind::NotRegular)
    );
}

#[test]
fn dpkg_status() {
    let t = TempDir::new("dpkg");
    let status = b"Package: bash\nStatus: install ok installed\nVersion: 5.2\n\n".repeat(20_000);
    std::fs::write(t.path("status"), &status).unwrap();
    let present = Config {
        dpkg_status: t.bytes("status"),
        ..config()
    };
    let (_, _, resp) = session_in(&present, Some(Hello::current()), &[Request::DpkgStatus]);
    let mut data = Vec::new();
    for r in &resp[..resp.len() - 1] {
        let Response::Chunk(b) = r else {
            panic!("unexpected {r:?}")
        };
        data.extend_from_slice(b);
    }
    assert_eq!(resp.last(), Some(&Response::ChunkEnd));
    assert!(resp.len() > 2, "sent in chunks");
    assert_eq!(data, status);

    let missing = Config {
        dpkg_status: t.bytes("missing"),
        ..config()
    };
    let (_, _, resp) = session_in(&missing, Some(Hello::current()), &[Request::DpkgStatus]);
    assert!(matches!(
        resp[..],
        [Response::Error {
            kind: ErrorKind::NotFound,
            ..
        }]
    ));
}

#[test]
fn access_times_never_change() {
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, SystemTime};
    let t = fixture("atime");
    // An access time a year in the past: with relatime, any plain read
    // would update it.
    let old = SystemTime::now() - Duration::from_secs(365 * 24 * 3600);
    let paths = ["", "sub", "sub/a.conf", "big", "secret/key"];
    for p in paths {
        let f = std::fs::File::open(if p.is_empty() { t.0.clone() } else { t.path(p) }).unwrap();
        f.set_times(std::fs::FileTimes::new().set_accessed(old))
            .unwrap();
    }
    let atimes = || -> Vec<i64> {
        paths
            .iter()
            .map(|p| {
                std::fs::symlink_metadata(if p.is_empty() { t.0.clone() } else { t.path(p) })
                    .unwrap()
                    .atime()
            })
            .collect()
    };
    let before = atimes();
    walk(WalkRequest {
        roots: vec![t.bytes("")],
        hash: true,
        max_hash_size: 1 << 20,
        ..Default::default()
    });
    read(t.bytes("sub/a.conf"), 100, vec![], false).unwrap();
    read(t.bytes("big"), 1 << 20, vec![], false).unwrap();
    assert_eq!(atimes(), before);
}

#[test]
fn hard_links_to_sensitive_files_are_sensitive() {
    let t = fixture("hardlink");
    std::fs::hard_link(t.path("secret/key"), t.path("innocent")).unwrap();
    let extra = vec![t.bytes("secret")];
    assert_eq!(
        read(t.bytes("innocent"), 100, extra.clone(), true),
        Err(ErrorKind::Sensitive)
    );
    let (entries, _, _) = walk(WalkRequest {
        roots: vec![t.bytes("innocent")],
        hash: true,
        max_hash_size: 100,
        sensitive: SensitivePolicy {
            extra,
            metadata: false,
        },
        ..Default::default()
    });
    assert!(entries[0].sensitive);
    assert_eq!(entries[0].hash, HashState::Withheld);
    assert_eq!(entries[0].size, None);
    // Without the pattern it is an ordinary file.
    assert_eq!(
        read(t.bytes("innocent"), 100, vec![], false),
        Ok(b"hunter2".to_vec())
    );
}
