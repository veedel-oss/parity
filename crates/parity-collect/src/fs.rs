//! Opening paths step by step from `/` with `openat` and `O_NOFOLLOW`, so a
//! symlink anywhere in the path is never followed (no `openat2`, which needs
//! Linux 5.6).

use std::ffi::CString;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd};

use crate::{path, sys};

// Opens a validated absolute path as an `O_PATH` directory descriptor: good
// for `openat` and `fstatat` below it, but it cannot be read, so it never
// changes an access time.
fn open_path_dir(p: &[u8]) -> io::Result<OwnedFd> {
    let mut fd = sys::open(c"/", sys::PATH_FLAGS)?;
    for c in path::components(p) {
        fd = sys::openat(fd.as_raw_fd(), &sys::cstring(c)?, sys::PATH_FLAGS)?;
    }
    Ok(fd)
}

// Opens a validated absolute path as a readable directory.
pub fn open_dir(p: &[u8]) -> io::Result<OwnedFd> {
    if p == b"/" {
        return sys::open(c"/", sys::DIR_FLAGS);
    }
    let (parent, name) = open_parent(p)?;
    sys::openat(parent.as_raw_fd(), &name, sys::DIR_FLAGS)
}

// Opens the parent directory of a validated path other than `/` (as
// `O_PATH`), and returns it with the last component.
pub fn open_parent(p: &[u8]) -> io::Result<(OwnedFd, CString)> {
    let cut = p.iter().rposition(|&b| b == b'/').expect("absolute path");
    let parent = if cut == 0 { &b"/"[..] } else { &p[..cut] };
    let name = sys::cstring(&p[cut + 1..])?;
    Ok((open_path_dir(parent)?, name))
}

pub enum OpenError {
    Io(io::Error),
    NotRegular,
    TooLarge(u64),
    // On a pseudo file system such as proc or sysfs, never read.
    PseudoFs,
}

impl From<io::Error> for OpenError {
    fn from(e: io::Error) -> Self {
        OpenError::Io(e)
    }
}

// Opens a regular file relative to `dir` without following symlinks and
// checks its size against `max`. Files on pseudo file systems are refused:
// reading them can block (`/proc/kmsg`) or have side effects.
pub fn open_regular_at(
    dir: &OwnedFd,
    name: &CString,
    max: u64,
) -> Result<(std::fs::File, libc::stat), OpenError> {
    let fd = match sys::openat(dir.as_raw_fd(), name, sys::FILE_FLAGS) {
        Ok(fd) => fd,
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Err(OpenError::NotRegular),
        Err(e) => return Err(e.into()),
    };
    let st = sys::fstat(fd.as_raw_fd())?;
    if st.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(OpenError::NotRegular);
    }
    if sys::is_pseudo_fs(fd.as_raw_fd())? {
        return Err(OpenError::PseudoFs);
    }
    let size = st.st_size as u64;
    if size > max {
        return Err(OpenError::TooLarge(size));
    }
    Ok((std::fs::File::from(fd), st))
}

// Opens a regular file by validated absolute path.
pub fn open_regular(p: &[u8], max: u64) -> Result<(std::fs::File, libc::stat), OpenError> {
    let (dir, name) = match open_parent(p) {
        Ok(x) => x,
        Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
            return Err(OpenError::NotRegular);
        }
        Err(e) => return Err(e.into()),
    };
    open_regular_at(&dir, &name, max)
}

// Reads a file in chunks, stopping with an error if it grows beyond `max`
// while being read. The outer error comes from `sink`, the inner one from
// reading the file.
pub fn read_chunks(
    file: &mut std::fs::File,
    max: u64,
    chunk: usize,
    mut sink: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<Result<(), OpenError>> {
    let mut buf = vec![0u8; chunk];
    let mut total = 0u64;
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => return Ok(Ok(())),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Ok(Err(e.into())),
        };
        total += n as u64;
        if total > max {
            return Ok(Err(OpenError::TooLarge(total)));
        }
        sink(&buf[..n])?;
    }
}
