//! Requests over an open connection.

use parity_proto::{
    Entry, ErrorKind, FileKind, ReadFileRequest, Request, Response, SensitivePolicy, WalkRequest,
};

use crate::error::{Result, bail};
use crate::packages::{self, Package};
use crate::ssh::Connection;

// Largest dpkg journal entry read. An entry holds the records of the
// packages one dpkg step changed, usually a few kilobytes.
const MAX_JOURNAL_ENTRY: u64 = 64 * 1024 * 1024;

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

    // The installed packages as dpkg sees them, `None` if the target has no
    // dpkg database: the status database and the pending entries of dpkg's
    // journal, in the order dpkg applies them (read after the status
    // database, as dpkg-query does).
    pub fn dpkg_packages(&mut self) -> Result<Option<Vec<Package>>> {
        self.send(&Request::DpkgStatus)?;
        let status = match self.chunks(u64::MAX)? {
            Ok(d) => d,
            Err((ErrorKind::NotFound, _)) => return Ok(None),
            Err((_, m)) => bail!("cannot read the dpkg database on {}: {m}", self.target),
        };

        let mut names = Vec::new();
        let mut error = None;
        let req = WalkRequest {
            roots: vec![packages::UPDATES_DIR.to_vec()],
            excludes: Vec::new(),
            hash: false,
            max_hash_size: 0,
            sensitive: SensitivePolicy::default(),
            batch_size: 500,
        };
        self.walk(req, |item| match item {
            WalkItem::Entries(v) => names.extend(v.into_iter().filter_map(|e| {
                let name = e
                    .path
                    .strip_prefix(packages::UPDATES_DIR)?
                    .strip_prefix(b"/")?;
                (e.kind == FileKind::Regular && packages::is_journal_entry(name))
                    .then(|| e.path.clone())
            })),
            WalkItem::Error { path, message } => {
                error.get_or_insert(format!("{}: {message}", String::from_utf8_lossy(&path)));
            }
        })?;
        if let Some(e) = error {
            bail!("cannot read the dpkg journal on {}: {e}", self.target);
        }
        names.sort();

        let mut updates = Vec::new();
        for path in names {
            let req = ReadFileRequest {
                path: path.clone(),
                max_size: MAX_JOURNAL_ENTRY,
                sensitive: SensitivePolicy::default(),
            };
            match self.read_file(req)? {
                Ok(d) => updates.push(d),
                // dpkg folded it into the status database meanwhile.
                Err((ErrorKind::NotFound, _)) => {}
                Err((_, m)) => bail!(
                    "cannot read the dpkg journal entry {} on {}: {m}",
                    String::from_utf8_lossy(&path),
                    self.target
                ),
            }
        }
        Ok(Some(packages::parse_database(&status, &updates)))
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
