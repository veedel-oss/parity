//! Thin wrappers around the system calls of collect mode:
//! `docs/collect-mode.md`, System calls.

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub const DIR_FLAGS: libc::c_int = libc::O_RDONLY
    | libc::O_DIRECTORY
    | libc::O_NOFOLLOW
    | libc::O_NOATIME
    | libc::O_CLOEXEC
    | libc::O_NOCTTY;

// For directories that are only passed through, never read.
pub const PATH_FLAGS: libc::c_int =
    libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

// `O_NONBLOCK` so that a file replaced by a FIFO between `stat` and `open`
// cannot block the walk. Regular files ignore it.
pub const FILE_FLAGS: libc::c_int = libc::O_RDONLY
    | libc::O_NOFOLLOW
    | libc::O_NOATIME
    | libc::O_CLOEXEC
    | libc::O_NOCTTY
    | libc::O_NONBLOCK;

pub fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

pub fn cstring(name: &[u8]) -> io::Result<CString> {
    CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn retry<T: PartialEq + Copy>(fail: T, mut f: impl FnMut() -> T) -> io::Result<T> {
    loop {
        let r = f();
        if r != fail {
            return Ok(r);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

pub fn open(path: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: path is a valid C string; the returned fd is owned by us.
    let fd = retry(-1, || unsafe { libc::open(path.as_ptr(), flags) })?;
    // SAFETY: fd is a freshly opened descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub fn openat(dir: RawFd, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: name is a valid C string; the returned fd is owned by us.
    let fd = retry(-1, || unsafe { libc::openat(dir, name.as_ptr(), flags) })?;
    // SAFETY: fd is a freshly opened descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub fn fstatat(dir: RawFd, name: &CStr) -> io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: st points to writable memory of the right size.
    retry(-1, || unsafe {
        libc::fstatat(
            dir,
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    // SAFETY: fstatat succeeded and filled st.
    Ok(unsafe { st.assume_init() })
}

pub fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: st points to writable memory of the right size.
    retry(-1, || unsafe { libc::fstat(fd, st.as_mut_ptr()) })?;
    // SAFETY: fstat succeeded and filled st.
    Ok(unsafe { st.assume_init() })
}

pub fn readlinkat(dir: RawFd, name: &CStr) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; 256];
    loop {
        // SAFETY: buf is writable for buf.len() bytes.
        let n = retry(-1, || unsafe {
            libc::readlinkat(dir, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len())
        })? as usize;
        if n < buf.len() {
            buf.truncate(n);
            return Ok(buf);
        }
        // Linux limits link targets to PATH_MAX; a longer one is refused so
        // a batch of entries always fits in a frame.
        if buf.len() > 4096 {
            return Err(io::Error::other("link target too long"));
        }
        buf.resize(4097, 0);
    }
}

// Names in a directory, without `.` and `..`, sorted bytewise.
pub fn read_dir_names(dir: &OwnedFd) -> io::Result<Vec<Vec<u8>>> {
    // SAFETY: dup of a valid fd; fdopendir takes ownership of the duplicate.
    let dup = retry(-1, || unsafe {
        libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0)
    })?;
    // SAFETY: dup is a valid directory fd we own.
    let d = unsafe { libc::fdopendir(dup) };
    if d.is_null() {
        let e = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so dup is still ours to close.
        unsafe { libc::close(dup) };
        return Err(e);
    }
    let mut names = Vec::new();
    let result = loop {
        // SAFETY: errno is thread-local; resetting it lets us tell the end
        // of the directory from an error.
        unsafe { *errno_location() = 0 };
        // SAFETY: d is a valid DIR*.
        let ent = unsafe { libc::readdir(d) };
        if ent.is_null() {
            // SAFETY: reading thread-local errno.
            let errno = unsafe { *errno_location() };
            break if errno == 0 {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(errno))
            };
        }
        // SAFETY: d_name is a NUL-terminated string inside the dirent.
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(name.to_vec());
        }
    };
    // SAFETY: d is valid and closed exactly once.
    unsafe { libc::closedir(d) };
    result?;
    names.sort();
    Ok(names)
}

unsafe fn errno_location() -> *mut libc::c_int {
    // SAFETY: provided by both glibc and musl.
    unsafe { libc::__errno_location() }
}

// To the hard limit, so deep trees can be walked with one descriptor per
// level.
pub fn raise_fd_limit() {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: lim is valid writable memory.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } == 0 && lim.rlim_cur < lim.rlim_max
    {
        lim.rlim_cur = lim.rlim_max;
        // SAFETY: lim is a valid rlimit.
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) };
    }
}

// File system types whose files are not data: reading them can block,
// change state or expose kernel internals.
const PSEUDO_FS: &[u32] = &[
    0x0000_9fa0, // proc
    0x6265_6572, // sysfs
    0x6462_6720, // debugfs
    0x7472_6163, // tracefs
    0x7363_6673, // securityfs
    0x0027_e0eb, // cgroup
    0x6367_7270, // cgroup2
    0xcafe_4a11, // bpf
    0x6165_676c, // pstore
    0x6265_6570, // configfs
    0xde5e_81e4, // efivarfs
    0x0000_1cd1, // devpts
    0x6573_5543, // fusectl
    0xf97c_ff8c, // selinuxfs
    0x4341_5d53, // smackfs
    0x4249_4e4d, // binfmt_misc
    0x1980_0202, // mqueue
    0x6e73_6673, // nsfs
];

pub fn is_pseudo_fs(fd: RawFd) -> io::Result<bool> {
    let mut st = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: st points to writable memory of the right size.
    retry(-1, || unsafe { libc::fstatfs(fd, st.as_mut_ptr()) })?;
    // SAFETY: fstatfs succeeded and filled st.
    Ok(is_pseudo_fs_type(unsafe { st.assume_init() }.f_type as u32))
}

// Whether a file system type (`f_type` of `statfs`) is a pseudo file system.
fn is_pseudo_fs_type(t: u32) -> bool {
    PSEUDO_FS.contains(&t)
}

// Whether nobody reads stdout any more (the SSH session has ended).
pub fn output_closed() -> bool {
    let mut fd = libc::pollfd {
        fd: 1,
        events: 0,
        revents: 0,
    };
    // SAFETY: fd is a valid pollfd; timeout 0 does not block.
    let r = unsafe { libc::poll(&mut fd, 1, 0) };
    r > 0 && fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
}

pub fn file_kind(mode: libc::mode_t) -> parity_proto::FileKind {
    use parity_proto::FileKind::*;
    match mode & libc::S_IFMT {
        libc::S_IFREG => Regular,
        libc::S_IFDIR => Directory,
        libc::S_IFLNK => Symlink,
        libc::S_IFCHR => CharDevice,
        libc::S_IFBLK => BlockDevice,
        libc::S_IFIFO => Fifo,
        libc::S_IFSOCK => Socket,
        _ => Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pseudo_fs_types() {
        // The kernel's magic numbers (linux/magic.h), as libc names them.
        for t in [
            libc::PROC_SUPER_MAGIC,
            libc::SYSFS_MAGIC,
            libc::DEBUGFS_MAGIC,
            libc::TRACEFS_MAGIC,
            libc::SECURITYFS_MAGIC,
            libc::CGROUP_SUPER_MAGIC,
            libc::CGROUP2_SUPER_MAGIC,
            libc::BPF_FS_MAGIC,
            libc::DEVPTS_SUPER_MAGIC,
        ] {
            assert!(is_pseudo_fs_type(t as u32), "{t:#x}");
        }
        // File systems with data.
        for t in [
            libc::EXT4_SUPER_MAGIC,
            libc::TMPFS_MAGIC,
            libc::XFS_SUPER_MAGIC,
            libc::BTRFS_SUPER_MAGIC,
            libc::NFS_SUPER_MAGIC,
            libc::OVERLAYFS_SUPER_MAGIC,
        ] {
            assert!(!is_pseudo_fs_type(t as u32), "{t:#x}");
        }
    }
}
