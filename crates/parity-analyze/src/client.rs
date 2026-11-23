//! Requests over an open connection.

use parity_proto::{Entry, ErrorKind, ReadFileRequest, Request, Response, WalkRequest};

use crate::error::{Result, bail};
use crate::ssh::Connection;

// File content, or the error collect mode reported for it.
pub type FileResult = std::result::Result<Vec<u8>, (ErrorKind, String)>;

pub enum WalkItem {
    Entries(Vec<Entry>),
    Error { path: Vec<u8>, message: String },
}

impl Connection {
    pub fn ping(&mut self) -> Result<()> {
        let nonce = u64::from(std::process::id()) << 16 | 0x5a;
        self.send(&Request::Ping { nonce })?;
        match self.recv()? {
            Response::Pong { nonce: n } if n == nonce => Ok(()),
            other => bail!("unexpected answer from {} to ping: {other:?}", self.target),
        }
    }

    // Collects chunks until the end marker. `Ok(Err(..))` is an error
    // reported by collect mode for this request.
    fn chunks(&mut self, max: u64) -> Result<FileResult> {
        let mut data = Vec::new();
        loop {
            match self.recv()? {
                Response::Chunk(b) => {
                    data.extend_from_slice(&b);
                    if data.len() as u64 > max {
                        bail!("{} sent more data than requested", self.target);
                    }
                }
                Response::ChunkEnd => return Ok(Ok(data)),
                Response::Error { kind, message } => return Ok(Err((kind, message))),
                other => bail!("unexpected answer from {}: {other:?}", self.target),
            }
        }
    }

    // Raw dpkg status database, `None` if the target has none.
    pub fn dpkg_status(&mut self) -> Result<Option<Vec<u8>>> {
        self.send(&Request::DpkgStatus)?;
        match self.chunks(u64::MAX)? {
            Ok(d) => Ok(Some(d)),
            Err((ErrorKind::NotFound, _)) => Ok(None),
            Err((_, m)) => bail!("cannot read the dpkg database on {}: {m}", self.target),
        }
    }

    pub fn read_file(&mut self, req: ReadFileRequest) -> Result<FileResult> {
        let max = req.max_size;
        self.send(&Request::ReadFile(req))?;
        self.chunks(max)
    }

    // Streams the walk to `sink` batch by batch.
    pub fn walk(&mut self, req: WalkRequest, mut sink: impl FnMut(WalkItem)) -> Result<()> {
        self.send(&Request::Walk(req))?;
        loop {
            match self.recv()? {
                Response::Entries(v) => sink(WalkItem::Entries(v)),
                Response::WalkError { path, message } => sink(WalkItem::Error { path, message }),
                Response::WalkEnd => return Ok(()),
                Response::Error { message, .. } => {
                    bail!("{} refused the request: {message}", self.target)
                }
                other => bail!("unexpected answer from {}: {other:?}", self.target),
            }
        }
    }
}
