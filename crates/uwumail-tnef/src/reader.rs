//! A cursor over untrusted bytes: every read is checked, nothing panics.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Eof;

pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Eof> {
        if n > self.remaining() {
            return Err(Eof);
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub fn skip(&mut self, n: usize) -> Result<(), Eof> {
        self.take(n).map(|_| ())
    }

    pub fn u8(&mut self) -> Result<u8, Eof> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Eof> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, Eof> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn i32(&mut self) -> Result<i32, Eof> {
        self.u32().map(|v| v as i32)
    }

    pub fn u64(&mut self) -> Result<u64, Eof> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    pub fn array16(&mut self) -> Result<[u8; 16], Eof> {
        let b = self.take(16)?;
        let mut a = [0u8; 16];
        a.copy_from_slice(b);
        Ok(a)
    }

    /// Skips the padding that brings a value of `len` bytes to a multiple of four. Padding that
    /// the stream leaves out at its very end is forgiven.
    pub fn pad4(&mut self, len: usize) {
        let pad = (4 - len % 4) % 4;
        let pad = pad.min(self.remaining());
        self.pos += pad;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_checked() {
        let mut r = Reader::new(&[1, 0, 2, 0, 0, 0, 9]);
        assert_eq!(r.u16(), Ok(1));
        assert_eq!(r.u32(), Ok(2));
        assert_eq!(r.u32(), Err(Eof));
        assert_eq!(r.u8(), Ok(9));
        assert!(r.is_empty());
        assert_eq!(r.take(usize::MAX), Err(Eof));
        r.pad4(3);
    }
}
