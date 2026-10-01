//! A cursor over a file's bytes for the format readers: little-endian values, counted
//! strings, and errors that say where the data ran out.

/// What went wrong while walking a file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    #[error("the data ends at {len:#x}, {what} at {at:#x} needs {need} more bytes")]
    Truncated { what: &'static str, at: usize, need: usize, len: usize },
    #[error("{what} at {at:#x} is {value}, more than the {max} any real file has")]
    TooMany { what: &'static str, at: usize, value: u32, max: u32 },
    #[error("{left} bytes left over at {at:#x}")]
    Trailing { at: usize, left: usize },
}

#[derive(Debug, Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn bytes(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], ReadError> {
        let end = self.pos.checked_add(n).filter(|&end| end <= self.data.len());
        let end = end.ok_or(ReadError::Truncated { what, at: self.pos, need: n, len: self.data.len() })?;
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    pub fn skip(&mut self, n: usize, what: &'static str) -> Result<(), ReadError> {
        self.bytes(n, what).map(|_| ())
    }

    pub fn array<const N: usize>(&mut self, what: &'static str) -> Result<[u8; N], ReadError> {
        Ok(self.bytes(N, what)?.try_into().expect("bytes() returns the length asked for"))
    }

    pub fn u8(&mut self, what: &'static str) -> Result<u8, ReadError> {
        Ok(self.array::<1>(what)?[0])
    }

    pub fn u16(&mut self, what: &'static str) -> Result<u16, ReadError> {
        Ok(u16::from_le_bytes(self.array(what)?))
    }

    pub fn i16(&mut self, what: &'static str) -> Result<i16, ReadError> {
        Ok(i16::from_le_bytes(self.array(what)?))
    }

    pub fn u32(&mut self, what: &'static str) -> Result<u32, ReadError> {
        Ok(u32::from_le_bytes(self.array(what)?))
    }

    pub fn f32(&mut self, what: &'static str) -> Result<f32, ReadError> {
        Ok(f32::from_le_bytes(self.array(what)?))
    }

    /// A half-precision float, widened.
    pub fn f16(&mut self, what: &'static str) -> Result<f32, ReadError> {
        Ok(half_to_f32(self.u16(what)?))
    }

    pub fn f32s<const N: usize>(&mut self, what: &'static str) -> Result<[f32; N], ReadError> {
        let mut out = [0.0; N];
        for value in &mut out {
            *value = self.f32(what)?;
        }
        Ok(out)
    }

    /// A u32 that counts something, refused if larger than `max`.
    pub fn count(&mut self, max: u32, what: &'static str) -> Result<usize, ReadError> {
        let at = self.pos;
        let value = self.u32(what)?;
        if value > max {
            return Err(ReadError::TooMany { what, at, value, max });
        }
        Ok(value as usize)
    }

    /// A u32 length followed by that many bytes of text.
    pub fn string(&mut self, what: &'static str) -> Result<String, ReadError> {
        let len = self.u32(what)? as usize;
        Ok(String::from_utf8_lossy(self.bytes(len, what)?).into_owned())
    }

    pub fn skip_string(&mut self, what: &'static str) -> Result<(), ReadError> {
        let len = self.u32(what)? as usize;
        self.skip(len, what)
    }

    /// A u32 count followed by that many strings.
    pub fn strings(&mut self, max: u32, what: &'static str) -> Result<Vec<String>, ReadError> {
        (0..self.count(max, what)?).map(|_| self.string(what)).collect()
    }

    /// Bytes at an offset from here, without moving.
    pub fn peek(&self, offset: usize, n: usize) -> Option<&'a [u8]> {
        self.data.get(self.pos.checked_add(offset)?..self.pos.checked_add(offset)?.checked_add(n)?)
    }

    /// Every byte must have been used.
    pub fn finish(&self) -> Result<(), ReadError> {
        match self.remaining() {
            0 => Ok(()),
            left => Err(ReadError::Trailing { at: self.pos, left }),
        }
    }
}

/// IEEE 754 half precision to single.
pub fn half_to_f32(half: u16) -> f32 {
    let sign = if half & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from(half >> 10 & 0x1f);
    let mantissa = f32::from(half & 0x3ff);
    sign * match exponent {
        0 => mantissa * 2f32.powi(-24),
        31 if mantissa == 0.0 => f32::INFINITY,
        31 => f32::NAN,
        e => (1.0 + mantissa / 1024.0) * 2f32.powi(e - 15),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_and_strings() {
        let mut data = vec![7, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12];
        data.extend_from_slice(&1.5f32.to_le_bytes());
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(b"abcXY");
        let mut r = Reader::new(&data);
        assert_eq!((r.u8("a").unwrap(), r.u16("b").unwrap(), r.u32("c").unwrap(), r.f32("d").unwrap()), (7, 0x1234, 0x1234_5678, 1.5));
        assert_eq!(r.peek(4, 3), Some(&b"abc"[..]));
        assert_eq!(r.string("name").unwrap(), "abc");
        assert_eq!((r.pos(), r.remaining()), (18, 2));
        assert_eq!(r.finish(), Err(ReadError::Trailing { at: 18, left: 2 }));
        assert_eq!(r.u32("tail"), Err(ReadError::Truncated { what: "tail", at: 18, need: 4, len: 20 }));
        r.skip(2, "rest").unwrap();
        assert_eq!(r.finish(), Ok(()));
        assert_eq!(r.peek(0, 1), None);
    }

    #[test]
    fn counts_are_limited() {
        let data = [5u32.to_le_bytes(), 600u32.to_le_bytes()].concat();
        let mut r = Reader::new(&data);
        assert_eq!(r.count(500, "bones"), Ok(5));
        assert_eq!(r.count(500, "bones"), Err(ReadError::TooMany { what: "bones", at: 4, value: 600, max: 500 }));
        let huge = u32::MAX.to_le_bytes();
        assert!(matches!(Reader::new(&huge).string("name"), Err(ReadError::Truncated { .. })));
    }

    #[test]
    fn halves() {
        for (half, expected) in [(0x0000, 0.0), (0x3c00, 1.0), (0xbc00, -1.0), (0x3800, 0.5), (0x4000, 2.0), (0x3555, 0.333_251_95), (0x0001, 5.960_464_5e-8)] {
            assert_eq!(half_to_f32(half), expected, "{half:#06x}");
        }
        assert_eq!(half_to_f32(0x7c00), f32::INFINITY);
        assert!(half_to_f32(0x7e00).is_nan());
    }
}
