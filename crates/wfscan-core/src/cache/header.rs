//! The common header that starts most files in an `H` cache.
//!
//! Layout, little-endian (Warframe and Soulframe):
//! - 16-byte hash;
//! - u32 count, then that many source paths, each a u32 length and that many bytes;
//! - u32 length and that many bytes of attribute text, followed by one NUL byte unless the
//!   length is 0;
//! - u32 whose low 16 bits are the file's type, which says how the rest of the H, B and F
//!   data is laid out.
//!
//! Not every H file has one, and nothing marks those that don't, so a file counts as
//! having a header only if all of this parses and the paths are printable text.
//!
//! Sources: LotusLib's description of the header; checked against a September 2026
//! Warframe install.

/// More source paths, or a longer one, than any real header has.
const MAX_PATHS: u32 = 1500;
const MAX_PATH_LEN: u32 = 1024;

const HASH_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonHeader {
    pub hash: [u8; HASH_LEN],
    /// Paths of the files this one was built from.
    pub sources: Vec<String>,
    pub attributes: String,
    /// The file type. Its meaning depends on the game and the package category.
    pub type_id: u32,
    /// The bits above the type in its u32.
    pub type_flags: u16,
    /// Bytes the header occupies; the type's own data starts here.
    pub len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    #[error("the file ends inside the header")]
    Truncated,
    #[error("{0} source paths is more than any real header has")]
    TooManyPaths(u32),
    #[error("a source path is not printable text")]
    BadPath,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], HeaderError> {
        let end = self.pos.checked_add(n).ok_or(HeaderError::Truncated)?;
        let b = self.data.get(self.pos..end).ok_or(HeaderError::Truncated)?;
        self.pos = end;
        Ok(b)
    }

    fn u32(&mut self) -> Result<u32, HeaderError> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
}

/// Parse the common header at the start of an H file.
pub fn parse(data: &[u8]) -> Result<CommonHeader, HeaderError> {
    let mut r = Reader { data, pos: 0 };
    let hash: [u8; HASH_LEN] = r.bytes(HASH_LEN)?.try_into().unwrap();
    let count = r.u32()?;
    if count > MAX_PATHS {
        return Err(HeaderError::TooManyPaths(count));
    }
    let mut sources = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let len = r.u32()?;
        if len > MAX_PATH_LEN {
            return Err(HeaderError::BadPath);
        }
        let path = r.bytes(len as usize)?;
        if !path.iter().all(|b| (0x20..0x7f).contains(b)) {
            return Err(HeaderError::BadPath);
        }
        sources.push(String::from_utf8_lossy(path).into_owned());
    }
    let attr_len = r.u32()?;
    let attributes = String::from_utf8_lossy(r.bytes(attr_len as usize)?).into_owned();
    if attr_len > 0 {
        r.bytes(1)?;
    }
    let ty = r.u32()?;
    Ok(CommonHeader { hash, sources, attributes, type_id: ty & 0xffff, type_flags: (ty >> 16) as u16, len: r.pos })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(sources: &[&str], attributes: &str, ty: u32) -> Vec<u8> {
        let mut h: Vec<u8> = (1..=16).collect();
        h.extend_from_slice(&(sources.len() as u32).to_le_bytes());
        for s in sources {
            h.extend_from_slice(&(s.len() as u32).to_le_bytes());
            h.extend_from_slice(s.as_bytes());
        }
        h.extend_from_slice(&(attributes.len() as u32).to_le_bytes());
        h.extend_from_slice(attributes.as_bytes());
        if !attributes.is_empty() {
            h.push(0);
        }
        h.extend_from_slice(&ty.to_le_bytes());
        h
    }

    #[test]
    fn with_and_without_attributes() {
        let mut data = build(&["/Lotus/A.fbx", "/Lotus/B.fbx"], "Key=1\n", 0x0001_010d);
        let len = data.len();
        data.extend_from_slice(b"body");
        let h = parse(&data).unwrap();
        assert_eq!(h.hash, <[u8; 16]>::try_from((1..=16).collect::<Vec<u8>>()).unwrap());
        assert_eq!(h.sources, ["/Lotus/A.fbx", "/Lotus/B.fbx"]);
        assert_eq!(h.attributes, "Key=1\n");
        assert_eq!((h.type_id, h.type_flags, h.len), (0x10d, 1, len));

        let bare = build(&[], "", 7);
        let h = parse(&bare).unwrap();
        assert_eq!((h.sources.len(), h.attributes.as_str(), h.type_id, h.len), (0, "", 7, 28));
    }

    #[test]
    fn rejects_what_is_not_a_header() {
        let good = build(&["/Lotus/A.fbx"], "x", 1);
        for cut in 0..good.len() {
            assert_eq!(parse(&good[..cut]), Err(HeaderError::Truncated), "cut at {cut}");
        }
        let mut many = good.clone();
        many[16..20].copy_from_slice(&5000u32.to_le_bytes());
        assert_eq!(parse(&many), Err(HeaderError::TooManyPaths(5000)));
        let mut long = good.clone();
        long[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse(&long), Err(HeaderError::BadPath));
        assert_eq!(parse(&build(&["bad\u{1}path"], "", 1)), Err(HeaderError::BadPath));
        let mut huge_attributes = build(&[], "", 1);
        huge_attributes[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse(&huge_attributes), Err(HeaderError::Truncated));
    }
}
