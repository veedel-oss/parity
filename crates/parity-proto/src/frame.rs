use std::io::{ErrorKind, Read, Write};

use crate::Error;

// Writes one frame: a 4-byte big-endian length, then the payload.
pub fn write_frame<W: Write + ?Sized>(w: &mut W, payload: &[u8]) -> Result<(), Error> {
    if payload.len() > crate::MAX_FRAME {
        return Err(Error::FrameTooLarge(payload.len()));
    }
    let len = (payload.len() as u32).to_be_bytes();
    w.write_all(&len)?;
    w.write_all(payload)?;
    w.flush()?;
    Ok(())
}

// Reads one frame. Returns `None` if the stream ends cleanly before a frame
// starts, an error if it ends in the middle of one or the frame is larger
// than `max`.
pub fn read_frame<R: Read + ?Sized>(r: &mut R, max: usize) -> Result<Option<Vec<u8>>, Error> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < len.len() {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(Error::Truncated),
            Ok(n) => got += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(Error::FrameTooLarge(len));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).map_err(|e| {
        if e.kind() == ErrorKind::UnexpectedEof {
            Error::Truncated
        } else {
            e.into()
        }
    })?;
    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"hello").unwrap();
        write_frame(&mut buf, b"").unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r, 100).unwrap().unwrap(), b"hello");
        assert_eq!(read_frame(&mut r, 100).unwrap().unwrap(), b"");
        assert!(read_frame(&mut r, 100).unwrap().is_none());
    }

    #[test]
    fn truncated_length() {
        let mut r = &[0u8, 0][..];
        assert!(matches!(read_frame(&mut r, 100), Err(Error::Truncated)));
    }

    #[test]
    fn truncated_payload() {
        let mut r = &[0u8, 0, 0, 5, b'a'][..];
        assert!(matches!(read_frame(&mut r, 100), Err(Error::Truncated)));
    }

    #[test]
    fn too_large() {
        let mut r = &[0u8, 0, 1, 0][..];
        assert!(matches!(
            read_frame(&mut r, 255),
            Err(Error::FrameTooLarge(256))
        ));
    }

    #[test]
    fn largest_frame() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &vec![0; crate::MAX_FRAME]).unwrap();
        assert!(matches!(
            write_frame(&mut Vec::new(), &vec![0; crate::MAX_FRAME + 1]),
            Err(Error::FrameTooLarge(_))
        ));
        let mut r = &buf[..];
        assert_eq!(
            read_frame(&mut r, crate::MAX_FRAME).unwrap().unwrap().len(),
            crate::MAX_FRAME
        );
    }

    // Returns the queued results of `read` in turn, then the bytes.
    struct Script(Vec<std::io::Result<()>>, &'static [u8]);

    impl Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.0.is_empty() {
                self.0.remove(0)?;
            }
            self.1.read(buf)
        }
    }

    #[test]
    fn retries_interrupted() {
        let mut r = Script(
            vec![Err(ErrorKind::Interrupted.into())],
            &[0, 0, 0, 1, b'x'],
        );
        assert_eq!(read_frame(&mut r, 100).unwrap().unwrap(), b"x");
    }

    #[test]
    fn read_error() {
        let mut r = Script(vec![Err(ErrorKind::BrokenPipe.into())], &[0, 0, 0, 1, b'x']);
        let e = read_frame(&mut r, 100).unwrap_err();
        assert!(matches!(&e, Error::Io(e) if e.kind() == ErrorKind::BrokenPipe));
        assert!(std::error::Error::source(&e).is_some());
        assert!(std::error::Error::source(&Error::Truncated).is_none());
    }
}
