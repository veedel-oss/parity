//! The encoding of `docs/architecture.md`, Protocol.

use crate::Error;

#[derive(Default)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn bool(&mut self, v: bool) {
        self.u8(v as u8);
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn raw(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    pub fn bytes(&mut self, v: &[u8]) {
        self.len(v.len());
        self.raw(v);
    }
    pub fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }
    pub fn len(&mut self, n: usize) {
        self.u32(u32::try_from(n).expect("length fits in u32"));
    }
    pub fn opt<T>(&mut self, v: &Option<T>, f: impl FnOnce(&mut Self, &T)) {
        match v {
            None => self.u8(0),
            Some(x) => {
                self.u8(1);
                f(self, x);
            }
        }
    }
    pub fn list<T>(&mut self, v: &[T], mut f: impl FnMut(&mut Self, &T)) {
        self.len(v.len());
        for x in v {
            f(self, x);
        }
    }
}

pub struct Decoder<'a> {
    buf: &'a [u8],
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }
    // Fails if bytes are left over.
    pub fn finish(self) -> Result<(), Error> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(Error::TrailingBytes)
        }
    }
    pub fn raw(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.buf.len() < n {
            return Err(Error::ShortMessage);
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.raw(N)?.try_into().expect("length checked"))
    }
    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.array::<1>()?[0])
    }
    pub fn bool(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            tag => Err(Error::UnknownTag { what: "bool", tag }),
        }
    }
    pub fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    pub fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    pub fn i64(&mut self) -> Result<i64, Error> {
        Ok(i64::from_be_bytes(self.array()?))
    }
    // Reads a length and checks that at least `min_item` bytes per item are
    // left, so a forged length cannot make the reader allocate a huge vector.
    pub fn len(&mut self, min_item: usize) -> Result<usize, Error> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_item.max(1)) > self.buf.len() {
            return Err(Error::ShortMessage);
        }
        Ok(n)
    }
    pub fn bytes(&mut self) -> Result<Vec<u8>, Error> {
        let n = self.len(1)?;
        Ok(self.raw(n)?.to_vec())
    }
    pub fn str(&mut self) -> Result<String, Error> {
        String::from_utf8(self.bytes()?).map_err(|_| Error::InvalidUtf8)
    }
    pub fn opt<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<Option<T>, Error> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(f(self)?)),
            tag => Err(Error::UnknownTag {
                what: "option",
                tag,
            }),
        }
    }
    pub fn list<T>(
        &mut self,
        min_item: usize,
        mut f: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let n = self.len(min_item)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(f(self)?);
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_round_trip() {
        let mut e = Encoder::new();
        e.u8(7);
        e.bool(true);
        e.u32(0xdead_beef);
        e.u64(u64::MAX);
        e.i64(-5);
        e.bytes(b"\xff\x00");
        e.str("grüß");
        e.opt(&Some(3u32), |e, v| e.u32(*v));
        e.opt(&None::<u32>, |e, v| e.u32(*v));
        e.list(&[1u64, 2], |e, v| e.u64(*v));
        let buf = e.finish();

        let mut d = Decoder::new(&buf);
        assert_eq!(d.u8().unwrap(), 7);
        assert!(d.bool().unwrap());
        assert_eq!(d.u32().unwrap(), 0xdead_beef);
        assert_eq!(d.u64().unwrap(), u64::MAX);
        assert_eq!(d.i64().unwrap(), -5);
        assert_eq!(d.bytes().unwrap(), b"\xff\x00");
        assert_eq!(d.str().unwrap(), "grüß");
        assert_eq!(d.opt(|d| d.u32()).unwrap(), Some(3));
        assert_eq!(d.opt(|d| d.u32()).unwrap(), None);
        assert_eq!(d.list(8, |d| d.u64()).unwrap(), vec![1, 2]);
        d.finish().unwrap();
    }

    #[test]
    fn forged_length_is_rejected() {
        let mut d = Decoder::new(&[0xff, 0xff, 0xff, 0xff, 1, 2]);
        assert!(matches!(d.bytes(), Err(Error::ShortMessage)));
        let mut d = Decoder::new(&[0, 0, 0, 2, 1, 2, 3, 4]);
        assert!(matches!(d.list(8, |d| d.u64()), Err(Error::ShortMessage)));
    }

    #[test]
    fn invalid_values() {
        assert!(Decoder::new(&[2]).bool().is_err());
        assert!(Decoder::new(&[0, 0, 0, 1, 0xff]).str().is_err());
        assert!(Decoder::new(&[1]).finish().is_err());
    }
}
