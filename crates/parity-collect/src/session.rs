//! One protocol session: hello exchange, then requests until the end.

use std::io::{self, Read, Write};

use parity_proto::{
    ErrorKind, Hello, MAX_FRAME, MAX_REQUEST_FRAME, ReadFileRequest, Request, Response, read_frame,
    write_frame,
};

use crate::fs::{self, OpenError};
use crate::sensitive::{self, Sensitive};
use crate::{path, sys, walk};

// Largest file content sent for a [`Request::ReadFile`].
pub const MAX_READ: u64 = 64 * 1024 * 1024;
// Largest dpkg status database sent.
pub const MAX_DPKG_STATUS: u64 = 256 * 1024 * 1024;
pub const DPKG_STATUS: &[u8] = b"/var/lib/dpkg/status";

// What collect mode reads that no request names. Fixed on a target
// ([`Config::target`]); tests pass their own.
pub struct Config {
    pub dpkg_status: Vec<u8>,
    pub sensitive: Vec<Vec<u8>>,
    // Whether nobody reads the output any more. Checked during long
    // requests, so collect mode exits soon after the session ends, not only
    // at its next write.
    pub output_closed: fn() -> bool,
}

impl Config {
    pub fn target() -> Config {
        Config {
            dpkg_status: DPKG_STATUS.to_vec(),
            sensitive: sensitive::builtin(),
            output_closed: sys::output_closed,
        }
    }
}

// How a session ended without error.
#[derive(Debug, PartialEq, Eq)]
pub enum End {
    // The CLI sent the end request.
    Request,
    // The input ended.
    Eof,
}

pub fn run(input: &mut dyn Read, output: &mut dyn Write, config: &Config) -> Result<End, String> {
    let werr = |e: io::Error| format!("cannot write to stdout: {e}");
    write_frame(output, &Hello::current().encode())
        .map_err(|e| format!("cannot write to stdout: {e}"))?;
    let mut send = |r: Response| -> io::Result<()> {
        write_frame(output, &r.encode()).map_err(|e| match e {
            parity_proto::Error::Io(e) => e,
            e => io::Error::other(e.to_string()),
        })
    };
    let hello = match read_frame(input, MAX_REQUEST_FRAME) {
        Ok(Some(f)) => Hello::decode(&f).map_err(|e| format!("invalid hello: {e}"))?,
        Ok(None) => return Ok(End::Eof),
        Err(e) => return Err(format!("cannot read hello: {e}")),
    };
    if hello.protocol != parity_proto::PROTOCOL_VERSION {
        return Err(format!(
            "protocol version mismatch: CLI {} (protocol {}), collect mode {} (protocol {})",
            hello.version,
            hello.protocol,
            env!("CARGO_PKG_VERSION"),
            parity_proto::PROTOCOL_VERSION
        ));
    }

    let mut cache = SensitiveCache::default();
    loop {
        let frame = match read_frame(input, MAX_REQUEST_FRAME) {
            Ok(Some(f)) => f,
            Ok(None) => return Ok(End::Eof),
            Err(e) => return Err(format!("cannot read request: {e}")),
        };
        let req = match Request::decode(&frame) {
            Ok(r) => r,
            Err(e) => {
                let message = format!("invalid request: {e}");
                send(Response::Error {
                    kind: ErrorKind::InvalidRequest,
                    message: message.clone(),
                })
                .map_err(werr)?;
                return Err(message);
            }
        };
        match req {
            Request::Ping { nonce } => send(Response::Pong { nonce }),
            Request::End => {
                send(Response::Bye).map_err(werr)?;
                return Ok(End::Request);
            }
            Request::DpkgStatus => send_file(&config.dpkg_status, MAX_DPKG_STATUS, None, &mut send),
            Request::Walk(w) => match cache.get(&config.sensitive, &w.sensitive.extra) {
                Ok(s) => walk::walk(&w, s, config.output_closed, &mut send),
                Err(e) => send(error(ErrorKind::InvalidRequest, e)),
            },
            Request::ReadFile(r) => match cache.get(&config.sensitive, &r.sensitive.extra) {
                Ok(s) => read_file(&r, s, &mut send),
                Err(e) => send(error(ErrorKind::InvalidRequest, e)),
            },
        }
        .map_err(werr)?;
    }
}

// The sensitive list of the last request, rebuilt only when the extra
// patterns change, since building it looks for hard links.
#[derive(Default)]
struct SensitiveCache {
    current: Option<(Vec<Vec<u8>>, Sensitive)>,
}

impl SensitiveCache {
    fn get(&mut self, builtin: &[Vec<u8>], extra: &[Vec<u8>]) -> Result<&Sensitive, String> {
        if self.current.as_ref().is_none_or(|(e, _)| e != extra) {
            self.current = Some((extra.to_vec(), Sensitive::new(builtin, extra)?));
        }
        Ok(&self.current.as_ref().expect("just set").1)
    }
}

fn error(kind: ErrorKind, message: impl Into<String>) -> Response {
    Response::Error {
        kind,
        message: message.into(),
    }
}

const SENSITIVE: &str = "the content of sensitive paths is never sent";

fn read_file(
    r: &ReadFileRequest,
    sensitive: &Sensitive,
    send: &mut dyn FnMut(Response) -> io::Result<()>,
) -> io::Result<()> {
    if let Err(e) = path::validate(&r.path) {
        return send(error(ErrorKind::InvalidRequest, e));
    }
    if sensitive.matches(&r.path) {
        return send(error(ErrorKind::Sensitive, SENSITIVE));
    }
    send_file(&r.path, r.max_size.min(MAX_READ), Some(sensitive), send)
}

fn send_file(
    p: &[u8],
    max: u64,
    sensitive: Option<&Sensitive>,
    send: &mut dyn FnMut(Response) -> io::Result<()>,
) -> io::Result<()> {
    let (mut file, st) = match fs::open_regular(p, max) {
        Ok(x) => x,
        Err(e) => return send(open_error(e)),
    };
    if sensitive.is_some_and(|s| s.is_link_of_sensitive(&st)) {
        return send(error(ErrorKind::Sensitive, SENSITIVE));
    }
    let chunk = parity_proto::CHUNK_SIZE.min(MAX_FRAME - 64);
    match fs::read_chunks(&mut file, max, chunk, |b| send(Response::Chunk(b.to_vec())))? {
        Ok(()) => send(Response::ChunkEnd),
        Err(e) => send(open_error(e)),
    }
}

fn open_error(e: OpenError) -> Response {
    match e {
        OpenError::NotRegular => error(
            ErrorKind::NotRegular,
            "not a regular file, or a path component is a symlink",
        ),
        OpenError::TooLarge(n) => error(ErrorKind::TooLarge, format!("file has {n} bytes")),
        OpenError::PseudoFs => error(
            ErrorKind::NotRegular,
            "file on a pseudo file system (proc, sysfs, ...), never read",
        ),
        OpenError::Io(e) if e.kind() == io::ErrorKind::NotFound => {
            error(ErrorKind::NotFound, "no such file")
        }
        OpenError::Io(e) => error(ErrorKind::Io, e.to_string()),
    }
}
