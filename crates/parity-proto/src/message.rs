//! Messages and their encoding.

use crate::codec::{Decoder, Encoder};
use crate::{Error, MAGIC};

// First message in each direction of a session.
//
// The encoding is fixed for all protocol versions: magic, u32 protocol
// version, then the program version as a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub protocol: u32,
    // Version of the program, for error messages.
    pub version: String,
}

impl Hello {
    pub fn current() -> Self {
        Hello {
            protocol: crate::PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.raw(&MAGIC);
        e.u32(self.protocol);
        e.str(&self.version);
        e.finish()
    }

    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut d = Decoder::new(buf);
        if d.raw(MAGIC.len()).map_err(|_| Error::BadMagic)? != MAGIC {
            return Err(Error::BadMagic);
        }
        let protocol = d.u32()?;
        let version = d.str()?;
        d.finish()?;
        Ok(Hello { protocol, version })
    }
}

// Requests from the CLI to collect mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    // Answered with [`Response::Pong`] carrying the same nonce.
    Ping { nonce: u64 },
    // Ends the session. Answered with [`Response::Bye`].
    End,
    // Raw content of the dpkg status database, as [`Response::Chunk`]s
    // followed by [`Response::ChunkEnd`].
    DpkgStatus,
    // File metadata below the given roots, as batches of
    // [`Response::Entries`] and [`Response::WalkError`], then
    // [`Response::WalkEnd`].
    Walk(WalkRequest),
    // Content of one regular file, as [`Response::Chunk`]s followed by
    // [`Response::ChunkEnd`], or an error.
    ReadFile(ReadFileRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WalkRequest {
    // Absolute paths to walk.
    pub roots: Vec<Vec<u8>>,
    // Absolute paths that are skipped together with everything below them.
    // `*` matches any run of bytes within one component.
    pub excludes: Vec<Vec<u8>>,
    // Hash regular files up to `max_hash_size` bytes.
    pub hash: bool,
    pub max_hash_size: u64,
    pub sensitive: SensitivePolicy,
    // Number of entries per [`Response::Entries`] message.
    pub batch_size: u32,
}

// How collect mode treats sensitive paths. The built-in list always applies.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SensitivePolicy {
    // Further patterns treated as sensitive, same syntax as the built-in list.
    pub extra: Vec<Vec<u8>>,
    // Report size, modification time, link target and hash for sensitive
    // paths too. Content is never sent, whatever this says.
    pub metadata: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFileRequest {
    pub path: Vec<u8>,
    pub max_size: u64,
    pub sensitive: SensitivePolicy,
}

// Answers from collect mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Pong { nonce: u64 },
    Bye,
    Chunk(Vec<u8>),
    ChunkEnd,
    Entries(Vec<Entry>),
    WalkError { path: Vec<u8>, message: String },
    WalkEnd,
    Error { kind: ErrorKind, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    // The request could not be decoded or makes no sense.
    InvalidRequest,
    NotFound,
    // The path is sensitive, its content is never sent.
    Sensitive,
    TooLarge,
    // The path is not a regular file, or a path component is a symlink.
    NotRegular,
    Io,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
    CharDevice,
    BlockDevice,
    Fifo,
    Socket,
    Unknown,
}

// Content hash of a regular file, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashState {
    // Not requested, or not a regular file.
    None,
    Sha256([u8; 32]),
    TooLarge,
    // Sensitive path, hash withheld.
    Withheld,
    Error(String),
    // On a pseudo file system such as proc or sysfs, never read.
    PseudoFs,
}

// Metadata of one path. Fields that are withheld for sensitive paths are
// `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: Vec<u8>,
    pub kind: FileKind,
    // Permission bits including setuid, setgid and sticky (`st_mode & 0o7777`).
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: Option<u64>,
    // Seconds and nanoseconds since the epoch.
    pub mtime: Option<(i64, u32)>,
    pub link_target: Option<Vec<u8>>,
    // Device number for character and block devices.
    pub rdev: Option<u64>,
    pub hash: HashState,
    pub sensitive: bool,
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        match self {
            Request::Ping { nonce } => {
                e.u8(1);
                e.u64(*nonce);
            }
            Request::End => e.u8(2),
            Request::DpkgStatus => e.u8(3),
            Request::Walk(w) => {
                e.u8(4);
                e.list(&w.roots, |e, p| e.bytes(p));
                e.list(&w.excludes, |e, p| e.bytes(p));
                e.bool(w.hash);
                e.u64(w.max_hash_size);
                encode_policy(&mut e, &w.sensitive);
                e.u32(w.batch_size);
            }
            Request::ReadFile(r) => {
                e.u8(5);
                e.bytes(&r.path);
                e.u64(r.max_size);
                encode_policy(&mut e, &r.sensitive);
            }
        }
        e.finish()
    }

    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut d = Decoder::new(buf);
        let r = match d.u8()? {
            1 => Request::Ping { nonce: d.u64()? },
            2 => Request::End,
            3 => Request::DpkgStatus,
            4 => Request::Walk(WalkRequest {
                roots: d.list(4, |d| d.bytes())?,
                excludes: d.list(4, |d| d.bytes())?,
                hash: d.bool()?,
                max_hash_size: d.u64()?,
                sensitive: decode_policy(&mut d)?,
                batch_size: d.u32()?,
            }),
            5 => Request::ReadFile(ReadFileRequest {
                path: d.bytes()?,
                max_size: d.u64()?,
                sensitive: decode_policy(&mut d)?,
            }),
            tag => {
                return Err(Error::UnknownTag {
                    what: "request",
                    tag,
                });
            }
        };
        d.finish()?;
        Ok(r)
    }
}

fn encode_policy(e: &mut Encoder, p: &SensitivePolicy) {
    e.list(&p.extra, |e, x| e.bytes(x));
    e.bool(p.metadata);
}

fn decode_policy(d: &mut Decoder) -> Result<SensitivePolicy, Error> {
    Ok(SensitivePolicy {
        extra: d.list(4, |d| d.bytes())?,
        metadata: d.bool()?,
    })
}

impl Response {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        match self {
            Response::Pong { nonce } => {
                e.u8(1);
                e.u64(*nonce);
            }
            Response::Bye => e.u8(2),
            Response::Chunk(b) => {
                e.u8(3);
                e.bytes(b);
            }
            Response::ChunkEnd => e.u8(4),
            Response::Entries(v) => {
                e.u8(5);
                e.list(v, encode_entry);
            }
            Response::WalkError { path, message } => {
                e.u8(6);
                e.bytes(path);
                e.str(message);
            }
            Response::WalkEnd => e.u8(7),
            Response::Error { kind, message } => {
                e.u8(8);
                e.u8(error_kind_tag(*kind));
                e.str(message);
            }
        }
        e.finish()
    }

    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut d = Decoder::new(buf);
        let r = match d.u8()? {
            1 => Response::Pong { nonce: d.u64()? },
            2 => Response::Bye,
            3 => Response::Chunk(d.bytes()?),
            4 => Response::ChunkEnd,
            5 => Response::Entries(d.list(16, decode_entry)?),
            6 => Response::WalkError {
                path: d.bytes()?,
                message: d.str()?,
            },
            7 => Response::WalkEnd,
            8 => Response::Error {
                kind: error_kind_from_tag(d.u8()?)?,
                message: d.str()?,
            },
            tag => {
                return Err(Error::UnknownTag {
                    what: "response",
                    tag,
                });
            }
        };
        d.finish()?;
        Ok(r)
    }
}

fn error_kind_tag(k: ErrorKind) -> u8 {
    match k {
        ErrorKind::InvalidRequest => 1,
        ErrorKind::NotFound => 2,
        ErrorKind::Sensitive => 3,
        ErrorKind::TooLarge => 4,
        ErrorKind::NotRegular => 5,
        ErrorKind::Io => 6,
    }
}

fn error_kind_from_tag(tag: u8) -> Result<ErrorKind, Error> {
    Ok(match tag {
        1 => ErrorKind::InvalidRequest,
        2 => ErrorKind::NotFound,
        3 => ErrorKind::Sensitive,
        4 => ErrorKind::TooLarge,
        5 => ErrorKind::NotRegular,
        6 => ErrorKind::Io,
        tag => {
            return Err(Error::UnknownTag {
                what: "error kind",
                tag,
            });
        }
    })
}

fn kind_tag(k: FileKind) -> u8 {
    match k {
        FileKind::Regular => 1,
        FileKind::Directory => 2,
        FileKind::Symlink => 3,
        FileKind::CharDevice => 4,
        FileKind::BlockDevice => 5,
        FileKind::Fifo => 6,
        FileKind::Socket => 7,
        FileKind::Unknown => 8,
    }
}

fn kind_from_tag(tag: u8) -> Result<FileKind, Error> {
    Ok(match tag {
        1 => FileKind::Regular,
        2 => FileKind::Directory,
        3 => FileKind::Symlink,
        4 => FileKind::CharDevice,
        5 => FileKind::BlockDevice,
        6 => FileKind::Fifo,
        7 => FileKind::Socket,
        8 => FileKind::Unknown,
        tag => {
            return Err(Error::UnknownTag {
                what: "file kind",
                tag,
            });
        }
    })
}

fn encode_entry(e: &mut Encoder, x: &Entry) {
    e.bytes(&x.path);
    e.u8(kind_tag(x.kind));
    e.u32(x.mode);
    e.u32(x.uid);
    e.u32(x.gid);
    e.opt(&x.size, |e, v| e.u64(*v));
    e.opt(&x.mtime, |e, (s, ns)| {
        e.i64(*s);
        e.u32(*ns);
    });
    e.opt(&x.link_target, |e, v| e.bytes(v));
    e.opt(&x.rdev, |e, v| e.u64(*v));
    match &x.hash {
        HashState::None => e.u8(0),
        HashState::Sha256(h) => {
            e.u8(1);
            e.raw(h);
        }
        HashState::TooLarge => e.u8(2),
        HashState::Withheld => e.u8(3),
        HashState::Error(m) => {
            e.u8(4);
            e.str(m);
        }
        HashState::PseudoFs => e.u8(5),
    }
    e.bool(x.sensitive);
}

fn decode_entry(d: &mut Decoder) -> Result<Entry, Error> {
    Ok(Entry {
        path: d.bytes()?,
        kind: kind_from_tag(d.u8()?)?,
        mode: d.u32()?,
        uid: d.u32()?,
        gid: d.u32()?,
        size: d.opt(|d| d.u64())?,
        mtime: d.opt(|d| Ok((d.i64()?, d.u32()?)))?,
        link_target: d.opt(|d| d.bytes())?,
        rdev: d.opt(|d| d.u64())?,
        hash: match d.u8()? {
            0 => HashState::None,
            1 => HashState::Sha256(d.raw(32)?.try_into().expect("length checked")),
            2 => HashState::TooLarge,
            3 => HashState::Withheld,
            4 => HashState::Error(d.str()?),
            5 => HashState::PseudoFs,
            tag => return Err(Error::UnknownTag { what: "hash", tag }),
        },
        sensitive: d.bool()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SensitivePolicy {
        SensitivePolicy {
            extra: vec![b"/srv/secret".to_vec()],
            metadata: true,
        }
    }

    fn entry() -> Entry {
        Entry {
            path: b"/etc/\xffpasswd".to_vec(),
            kind: FileKind::Regular,
            mode: 0o4755,
            uid: 0,
            gid: 42,
            size: Some(1234),
            mtime: Some((-1, 999_999_999)),
            link_target: None,
            rdev: Some(7),
            hash: HashState::Sha256([9; 32]),
            sensitive: false,
        }
    }

    #[test]
    fn hello_round_trip() {
        let h = Hello::current();
        assert_eq!(Hello::decode(&h.encode()).unwrap(), h);
    }

    #[test]
    fn hello_rejects_foreign_data() {
        assert!(matches!(
            Hello::decode(b"Welcome to Debian"),
            Err(Error::BadMagic)
        ));
        assert!(matches!(Hello::decode(b"PR"), Err(Error::BadMagic)));
    }

    #[test]
    fn hello_encoding_is_stable() {
        let h = Hello {
            protocol: 1,
            version: "0.1.0".into(),
        };
        assert_eq!(h.encode(), b"PRTY\0\0\0\x01\0\0\0\x050.1.0");
    }

    #[test]
    fn requests_round_trip() {
        let all = [
            Request::Ping { nonce: 42 },
            Request::End,
            Request::DpkgStatus,
            Request::Walk(WalkRequest {
                roots: vec![b"/etc".to_vec()],
                excludes: vec![b"/etc/x".to_vec(), b"/proc".to_vec()],
                hash: true,
                max_hash_size: 1 << 30,
                sensitive: policy(),
                batch_size: 512,
            }),
            Request::ReadFile(ReadFileRequest {
                path: b"/etc/hosts".to_vec(),
                max_size: 100,
                sensitive: SensitivePolicy::default(),
            }),
        ];
        for r in all {
            assert_eq!(Request::decode(&r.encode()).unwrap(), r);
        }
    }

    #[test]
    fn responses_round_trip() {
        let mut other = entry();
        other.kind = FileKind::Symlink;
        other.size = None;
        other.mtime = None;
        other.link_target = Some(b"../target".to_vec());
        other.rdev = None;
        other.hash = HashState::Error("denied".into());
        other.sensitive = true;
        let mut hashes = vec![entry(), other];
        for h in [
            HashState::None,
            HashState::TooLarge,
            HashState::Withheld,
            HashState::PseudoFs,
        ] {
            let mut x = entry();
            x.hash = h;
            hashes.push(x);
        }
        let all = [
            Response::Pong { nonce: 1 },
            Response::Bye,
            Response::Chunk(vec![0, 1, 2]),
            Response::ChunkEnd,
            Response::Entries(hashes),
            Response::Entries(vec![]),
            Response::WalkError {
                path: b"/x".to_vec(),
                message: "permission denied".into(),
            },
            Response::WalkEnd,
            Response::Error {
                kind: ErrorKind::Sensitive,
                message: "no".into(),
            },
        ];
        for r in all {
            assert_eq!(Response::decode(&r.encode()).unwrap(), r);
        }
    }

    #[test]
    fn all_kinds_round_trip() {
        for k in [
            FileKind::Regular,
            FileKind::Directory,
            FileKind::Symlink,
            FileKind::CharDevice,
            FileKind::BlockDevice,
            FileKind::Fifo,
            FileKind::Socket,
            FileKind::Unknown,
        ] {
            assert_eq!(kind_from_tag(kind_tag(k)).unwrap(), k);
        }
        for k in [
            ErrorKind::InvalidRequest,
            ErrorKind::NotFound,
            ErrorKind::Sensitive,
            ErrorKind::TooLarge,
            ErrorKind::NotRegular,
            ErrorKind::Io,
        ] {
            assert_eq!(error_kind_from_tag(error_kind_tag(k)).unwrap(), k);
        }
    }

    #[test]
    fn unknown_tags_and_trailing_bytes() {
        assert!(Request::decode(&[99]).is_err());
        assert!(Response::decode(&[99]).is_err());
        assert!(Request::decode(&[2, 0]).is_err());
        assert!(Request::decode(&[]).is_err());
    }

    #[test]
    fn truncated_messages_never_panic() {
        let full = Response::Entries(vec![entry(), entry()]).encode();
        for n in 0..full.len() {
            assert!(Response::decode(&full[..n]).is_err());
        }
        let full = Request::Walk(WalkRequest {
            roots: vec![b"/".to_vec()],
            ..Default::default()
        })
        .encode();
        for n in 0..full.len() {
            assert!(Request::decode(&full[..n]).is_err());
        }
    }
}
