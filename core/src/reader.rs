//! Bounded big-endian reader over the raw font bytes.
//! Every access is bounds-checked; out-of-range reads return [`Error::BadOffset`]
//! instead of panicking, so a malformed font can never trap the WASM runtime.

use crate::Error;

#[derive(Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    #[inline]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[inline]
    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    #[inline]
    pub fn position(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    #[inline]
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    #[inline]
    pub fn skip(&mut self, n: usize) -> Result<(), Error> {
        let next = self.pos.checked_add(n).ok_or(Error::BadOffset)?;
        if next > self.data.len() {
            return Err(Error::BadOffset);
        }
        self.pos = next;
        Ok(())
    }

    #[inline]
    pub fn seek(&mut self, pos: usize) -> Result<(), Error> {
        if pos > self.data.len() {
            return Err(Error::BadOffset);
        }
        self.pos = pos;
        Ok(())
    }

    #[inline]
    pub fn read_u8(&mut self) -> Result<u8, Error> {
        let b = *self.data.get(self.pos).ok_or(Error::BadOffset)?;
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    pub fn peek_u8(&self) -> Result<u8, Error> {
        self.data.get(self.pos).copied().ok_or(Error::BadOffset)
    }

    #[inline]
    pub fn read_i8(&mut self) -> Result<i8, Error> {
        Ok(self.read_u8()? as i8)
    }

    #[inline]
    pub fn read_u16(&mut self) -> Result<u16, Error> {
        if self.pos + 2 > self.data.len() {
            return Err(Error::BadOffset);
        }
        let v = u16::from_be_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    #[inline]
    pub fn read_i16(&mut self) -> Result<i16, Error> {
        Ok(self.read_u16()? as i16)
    }

    #[inline]
    pub fn read_u32(&mut self) -> Result<u32, Error> {
        if self.pos + 4 > self.data.len() {
            return Err(Error::BadOffset);
        }
        let v = u32::from_be_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(v)
    }

    #[inline]
    pub fn read_i32(&mut self) -> Result<i32, Error> {
        Ok(self.read_u32()? as i32)
    }

    /// 16.16 fixed point
    #[inline]
    pub fn read_fixed(&mut self) -> Result<i32, Error> {
        self.read_i32()
    }

    /// 2.14 fixed point
    #[inline]
    pub fn read_f2dot14(&mut self) -> Result<f32, Error> {
        Ok(self.read_i16()? as f32 / 16384.0)
    }

    #[inline]
    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::BadOffset)?;
        if end > self.data.len() {
            return Err(Error::BadOffset);
        }
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// A sub-slice spanning `[offset, offset+len)` without moving the cursor.
    #[inline]
    pub fn slice(&self, offset: usize, len: usize) -> Result<&'a [u8], Error> {
        let end = offset.checked_add(len).ok_or(Error::BadOffset)?;
        if end > self.data.len() {
            return Err(Error::BadOffset);
        }
        Ok(&self.data[offset..end])
    }

    /// Tagged 4-byte table identifier, e.g. b"cmap".
    #[inline]
    pub fn read_tag(&mut self) -> Result<[u8; 4], Error> {
        let s = self.read_bytes(4)?;
        Ok([s[0], s[1], s[2], s[3]])
    }
}
