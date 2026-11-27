//! Walks directory trees and streams file metadata in batches.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};

use parity_proto::{Entry, ErrorKind, FileKind, HashState, Response, WalkRequest};
use xxhash_rust::xxh3::Xxh3;

use crate::fs::{self, OpenError};
use crate::pattern::{self, Pattern};
use crate::sensitive::Sensitive;
use crate::{path, sys};

pub const MAX_ROOTS: usize = 64;
pub const MAX_EXCLUDES: usize = 4096;
pub const MAX_BATCH: u32 = 1000;
pub const DEFAULT_BATCH: u32 = 500;

pub type Sink<'a> = dyn FnMut(Response) -> io::Result<()> + 'a;

// Returns only write errors and `closed`; problems with the request or the
// file system are reported over `out`.
pub fn walk(
    req: &WalkRequest,
    sensitive: &Sensitive,
    closed: fn() -> bool,
    out: &mut Sink,
) -> io::Result<()> {
    let excludes = match check(req) {
        Ok(x) => x,
        Err(message) => {
            return out(Response::Error {
                kind: ErrorKind::InvalidRequest,
                message,
            });
        }
    };
    let batch_size = match req.batch_size {
        0 => DEFAULT_BATCH,
        n => n.min(MAX_BATCH),
    } as usize;
    let mut w = Walker {
        req,
        excludes,
        sensitive,
        batch: Vec::with_capacity(batch_size),
        batch_size,
        closed,
        out,
    };
    for root in &req.roots {
        w.root(root)?;
    }
    w.flush()?;
    (w.out)(Response::WalkEnd)
}

// Checks the limits and paths of a request and returns its excludes as
// patterns.
fn check(req: &WalkRequest) -> Result<Vec<Pattern>, String> {
    if req.roots.len() > MAX_ROOTS {
        return Err(format!("more than {MAX_ROOTS} roots"));
    }
    if req.excludes.len() > MAX_EXCLUDES {
        return Err(format!("more than {MAX_EXCLUDES} excludes"));
    }
    for p in &req.roots {
        path::validate(p)?;
    }
    req.excludes.iter().map(|p| Pattern::new(p)).collect()
}

struct Walker<'a, 'b> {
    req: &'a WalkRequest,
    excludes: Vec<Pattern>,
    sensitive: &'a Sensitive,
    batch: Vec<Entry>,
    batch_size: usize,
    closed: fn() -> bool,
    out: &'a mut Sink<'b>,
}

impl Walker<'_, '_> {
    fn excluded(&self, p: &[u8]) -> bool {
        pattern::any_covers(&self.excludes, p)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let batch = std::mem::replace(&mut self.batch, Vec::with_capacity(self.batch_size));
        (self.out)(Response::Entries(batch))
    }

    fn push(&mut self, e: Entry) -> io::Result<()> {
        // Without this, a walk would go on until the batch is full and its
        // write fails, which can take long on a slow disk.
        if (self.closed)() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "session closed"));
        }
        self.batch.push(e);
        if self.batch.len() >= self.batch_size {
            self.flush()?;
        }
        Ok(())
    }

    fn error(&mut self, p: &[u8], e: impl std::fmt::Display) -> io::Result<()> {
        self.flush()?;
        (self.out)(Response::WalkError {
            path: p.to_vec(),
            message: e.to_string(),
        })
    }

    fn root(&mut self, root: &[u8]) -> io::Result<()> {
        if self.excluded(root) {
            return Ok(());
        }
        if root == b"/" {
            let fd = match fs::open_dir(root) {
                Ok(fd) => fd,
                Err(e) => return self.error(root, e),
            };
            let st = match sys::fstat(fd.as_raw_fd()) {
                Ok(st) => st,
                Err(e) => return self.error(root, e),
            };
            let e = self.entry(None, root.to_vec(), &st);
            self.push(e)?;
            return self.descend(&fd, root);
        }
        match fs::open_parent(root) {
            Ok((dir, name)) => self.visit(&dir, &name, root.to_vec()),
            // A missing root shows up as missing in the comparison.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
                self.error(root, "a path component is a symlink or not a directory")
            }
            Err(e) => self.error(root, e),
        }
    }

    fn visit(&mut self, dir: &OwnedFd, name: &CStr, p: Vec<u8>) -> io::Result<()> {
        let st = match sys::fstatat(dir.as_raw_fd(), name) {
            Ok(st) => st,
            // Vanished since the directory was read.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return self.error(&p, e),
        };
        let e = self.entry(Some((dir, name)), p, &st);
        let is_dir = e.kind == FileKind::Directory;
        let p = e.path.clone();
        self.push(e)?;
        if is_dir {
            match sys::openat(dir.as_raw_fd(), name, sys::DIR_FLAGS) {
                Ok(fd) => self.descend(&fd, &p)?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => self.error(&p, e)?,
            }
        }
        Ok(())
    }

    fn descend(&mut self, fd: &OwnedFd, p: &[u8]) -> io::Result<()> {
        let names = match sys::read_dir_names(fd) {
            Ok(n) => n,
            Err(e) => return self.error(p, e),
        };
        for name in names {
            let child = path::join(p, &name);
            if self.excluded(&child) {
                continue;
            }
            if child.len() > path::MAX_PATH {
                self.error(&child, "path too long")?;
                continue;
            }
            match sys::cstring(&name) {
                Ok(c) => self.visit(fd, &c, child)?,
                Err(e) => self.error(&child, e)?,
            }
        }
        Ok(())
    }

    // `at` is the parent directory and name, `None` for `/`.
    fn entry(&mut self, at: Option<(&OwnedFd, &CStr)>, p: Vec<u8>, st: &libc::stat) -> Entry {
        let kind = sys::file_kind(st.st_mode);
        let sensitive = self.sensitive.matches(&p) || self.sensitive.is_link_of_sensitive(st);
        let reveal = !sensitive || self.req.sensitive.metadata;
        let link_target = match (kind, at, reveal) {
            (FileKind::Symlink, Some((dir, name)), true) => {
                sys::readlinkat(dir.as_raw_fd(), name).ok()
            }
            _ => None,
        };
        let hash = match (kind, at) {
            (FileKind::Regular, Some((dir, name))) if self.req.hash => {
                // Never for sensitive paths, not even with the metadata
                // opt-in: XXH3 is not built to be hard to reverse, so the
                // hash of a short secret could give the secret away.
                if sensitive {
                    HashState::Withheld
                } else if st.st_size as u64 > self.req.max_hash_size {
                    HashState::TooLarge
                } else {
                    hash_file(dir, name, self.req.max_hash_size, self.closed)
                }
            }
            _ => HashState::None,
        };
        Entry {
            kind,
            mode: st.st_mode & 0o7777,
            uid: st.st_uid,
            gid: st.st_gid,
            size: reveal.then_some(st.st_size as u64),
            mtime: reveal.then_some((st.st_mtime, st.st_mtime_nsec as u32)),
            link_target,
            rdev: (reveal && matches!(kind, FileKind::CharDevice | FileKind::BlockDevice))
                .then_some(st.st_rdev),
            hash,
            sensitive,
            path: p,
        }
    }
}

fn hash_file(dir: &OwnedFd, name: &CStr, max: u64, closed: fn() -> bool) -> HashState {
    let name = name.to_owned();
    let (mut file, _) = match fs::open_regular_at(dir, &name, max) {
        Ok(x) => x,
        Err(OpenError::TooLarge(_)) => return HashState::TooLarge,
        Err(OpenError::NotRegular) => return HashState::Error("not a regular file".into()),
        Err(OpenError::PseudoFs) => return HashState::PseudoFs,
        Err(OpenError::Io(e)) => return HashState::Error(e.to_string()),
    };
    let mut h = Xxh3::new();
    let mut chunks = 0u32;
    match fs::read_chunks(&mut file, max, 64 * 1024, |b| {
        h.update(b);
        // Large files take a while: stop early if the session has ended (the
        // walk then ends at the next entry).
        chunks = chunks.wrapping_add(1);
        if chunks.is_multiple_of(256) && closed() {
            return Err(io::Error::other("session closed"));
        }
        Ok(())
    }) {
        Ok(Ok(())) => HashState::Xxh3(h.digest128().to_be_bytes()),
        Ok(Err(OpenError::TooLarge(_))) => HashState::TooLarge,
        Ok(Err(OpenError::NotRegular)) => HashState::Error("not a regular file".into()),
        Ok(Err(OpenError::PseudoFs)) => HashState::PseudoFs,
        Ok(Err(OpenError::Io(e))) | Err(e) => HashState::Error(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits() {
        let req = |roots, excludes| WalkRequest {
            roots: vec![b"/x".to_vec(); roots],
            excludes: vec![b"/y".to_vec(); excludes],
            ..Default::default()
        };
        assert!(check(&req(MAX_ROOTS, MAX_EXCLUDES)).is_ok());
        assert!(check(&req(MAX_ROOTS + 1, 0)).is_err());
        assert!(check(&req(1, MAX_EXCLUDES + 1)).is_err());
    }
}
