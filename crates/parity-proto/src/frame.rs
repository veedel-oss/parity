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
}
