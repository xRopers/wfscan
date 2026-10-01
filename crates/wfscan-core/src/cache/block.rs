//! Turning one TOC entry's bytes in a `.cache` file into the file's contents.
//!
//! Warframe since the September 2020 "Ensmallening", and Soulframe:
//! - An entry whose compressed size equals its size is stored as is.
//! - Otherwise it is a run of blocks, each with an 8-byte header holding two big-endian
//!   u32s: the first has its top byte `0x80` and the block's compressed size in bits 2-25;
//!   the second has the decompressed size in bits 5-28 and `1` in its low 4 bits. The
//!   blocks' headers and data add up to the entry's compressed size exactly.
//! - Entries without such a header are a single block covering the whole entry.
//! - A block starting with `0x8C` is Oodle (a Kraken-family block header with the reset
//!   bit); a block as large as its output is stored; anything else is LZF.
//!
//! Sources: the header bit layout and the Oodle/LZF rule come from LotusLib's description
//! of the format; the sizes adding up, and the layout itself, were checked against every
//! entry of a September 2026 Warframe install (`wfscan index --hash`).

use super::lzf::{self, LzfError};

/// First byte of an Oodle block as the game writes them.
pub const OODLE_MAGIC: u8 = 0x8c;

const HEADER_LEN: usize = 8;

/// Something that decodes Oodle data: the real library, or a stand-in in tests.
pub trait OodleDecoder: Sync {
    /// Decode all of `comp` into exactly `out.len()` bytes.
    fn decode(&self, comp: &[u8], out: &mut [u8]) -> Result<(), String>;
}

/// How many blocks of each kind were decoded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct BlockCounts {
    pub oodle: u64,
    pub lzf: u64,
    pub stored: u64,
}

impl std::ops::AddAssign for BlockCounts {
    fn add_assign(&mut self, o: Self) {
        self.oodle += o.oodle;
        self.lzf += o.lzf;
        self.stored += o.stored;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlockError {
    #[error("no block header at {offset:#x}")]
    NoHeader { offset: usize },
    #[error("block at {offset:#x} is empty")]
    Empty { offset: usize },
    #[error("block at {offset:#x}: {compressed} compressed bytes run past the entry's end")]
    PastInput { offset: usize, compressed: usize },
    #[error("block at {offset:#x}: {size} bytes run past the file's size")]
    PastOutput { offset: usize, size: usize },
    #[error("block at {offset:#x} is Oodle, and no Oodle library is loaded")]
    NoOodle { offset: usize },
    #[error("block at {offset:#x}: Oodle: {reason}")]
    Oodle { offset: usize, reason: String },
    #[error("block at {offset:#x}: LZF: {source}")]
    Lzf { offset: usize, source: LzfError },
    #[error("{left} bytes left over after the last block")]
    Trailing { left: usize },
}

/// The (compressed, decompressed) sizes in a block header, if `data` starts with one.
fn block_sizes(data: &[u8]) -> Option<(usize, usize)> {
    let h: &[u8; HEADER_LEN] = data.get(..HEADER_LEN)?.try_into().ok()?;
    if h[0] != 0x80 || h[7] & 0x0f != 1 {
        return None;
    }
    let first = u32::from_be_bytes([h[0], h[1], h[2], h[3]]);
    let second = u32::from_be_bytes([h[4], h[5], h[6], h[7]]);
    Some(((first >> 2 & 0xff_ffff) as usize, (second >> 5 & 0xff_ffff) as usize))
}

/// Whether `data`, an entry's bytes, starts with a block header.
pub fn has_block_header(data: &[u8]) -> bool {
    block_sizes(data).is_some()
}

/// Decode one entry: `data` is its `compressed_size` bytes from the cache, `size` its
/// decompressed size. Every input byte must be used and every output byte produced.
pub fn decompress(data: &[u8], size: usize, oodle: Option<&dyn OodleDecoder>) -> Result<(Vec<u8>, BlockCounts), BlockError> {
    let mut counts = BlockCounts::default();
    if data.len() == size {
        counts.stored += u64::from(size > 0);
        return Ok((data.to_vec(), counts));
    }
    let mut out = vec![0u8; size];
    let (mut pos, mut produced) = (0, 0);
    while produced < size {
        let offset = pos;
        let (compressed, block_size) = match block_sizes(&data[pos..]) {
            Some(sizes) => {
                pos += HEADER_LEN;
                sizes
            }
            None if pos == 0 => (data.len(), size),
            None => return Err(BlockError::NoHeader { offset }),
        };
        if compressed == 0 || block_size == 0 {
            return Err(BlockError::Empty { offset });
        }
        let block = data.get(pos..pos + compressed).ok_or(BlockError::PastInput { offset, compressed })?;
        let target = out.get_mut(produced..produced + block_size).ok_or(BlockError::PastOutput { offset, size: block_size })?;
        decode_block(block, target, offset, oodle, &mut counts)?;
        pos += compressed;
        produced += block_size;
    }
    if pos != data.len() {
        return Err(BlockError::Trailing { left: data.len() - pos });
    }
    Ok((out, counts))
}

fn decode_block(
    block: &[u8],
    out: &mut [u8],
    offset: usize,
    oodle: Option<&dyn OodleDecoder>,
    counts: &mut BlockCounts,
) -> Result<(), BlockError> {
    if block[0] == OODLE_MAGIC {
        let oodle = oodle.ok_or(BlockError::NoOodle { offset })?;
        match oodle.decode(block, out) {
            Ok(()) => {
                counts.oodle += 1;
                return Ok(());
            }
            // A stored block may happen to start with the same byte.
            Err(_) if block.len() == out.len() => {}
            Err(reason) => return Err(BlockError::Oodle { offset, reason }),
        }
    }
    if block.len() == out.len() {
        out.copy_from_slice(block);
        counts.stored += 1;
    } else {
        lzf::decompress(block, out).map_err(|source| BlockError::Lzf { offset, source })?;
        counts.lzf += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(compressed: usize, size: usize) -> [u8; 8] {
        let mut h = [0u8; 8];
        h[..4].copy_from_slice(&(0x8000_0000 | (compressed as u32) << 2).to_be_bytes());
        h[4..].copy_from_slice(&((size as u32) << 5 | 1).to_be_bytes());
        h
    }

    fn block(payload: &[u8], size: usize) -> Vec<u8> {
        let mut b = header(payload.len(), size).to_vec();
        b.extend_from_slice(payload);
        b
    }

    /// "Oodle" for tests: the magic byte, then the data with every bit flipped.
    struct Flip;

    impl OodleDecoder for Flip {
        fn decode(&self, comp: &[u8], out: &mut [u8]) -> Result<(), String> {
            if comp.len() != out.len() + 1 {
                return Err("wrong size".into());
            }
            out.iter_mut().zip(&comp[1..]).for_each(|(o, c)| *o = !c);
            Ok(())
        }
    }

    fn flip(data: &[u8]) -> Vec<u8> {
        std::iter::once(OODLE_MAGIC).chain(data.iter().map(|b| !b)).collect()
    }

    // "abc", then 9 bytes from distance 3.
    const LZF: [u8; 7] = [2, b'a', b'b', b'c', 7 << 5, 0, 2];
    const LZF_OUT: &[u8] = b"abcabcabcabc";

    #[test]
    fn header_sizes_match_a_real_entry() {
        // DropshipActions.lua in H.Font: 1023 bytes in the cache, 2402 decompressed.
        assert_eq!(block_sizes(&[0x80, 0x00, 0x0f, 0xdc, 0x00, 0x01, 0x2c, 0x41]), Some((1015, 2402)));
        assert_eq!(block_sizes(&header(1015, 2402)), Some((1015, 2402)));
        assert_eq!(block_sizes(&[0x81, 0, 0, 0, 0, 0, 0, 1]), None);
        assert_eq!(block_sizes(&[0x80, 0, 0, 0, 0, 0, 0, 2]), None);
        assert_eq!(block_sizes(&[0x80, 0, 0]), None);
    }

    #[test]
    fn stored_entry() {
        assert_eq!(decompress(b"hello", 5, None).unwrap(), (b"hello".to_vec(), BlockCounts { stored: 1, ..Default::default() }));
        assert_eq!(decompress(b"", 0, None).unwrap(), (Vec::new(), BlockCounts::default()));
    }

    #[test]
    fn blocks_of_each_kind() {
        let mut entry = block(&LZF, LZF_OUT.len());
        entry.extend(block(b"stored!", 7));
        entry.extend(block(&flip(b"oodle"), 5));
        let (out, counts) = decompress(&entry, LZF_OUT.len() + 12, Some(&Flip)).unwrap();
        assert_eq!(out, [LZF_OUT, b"stored!", b"oodle"].concat());
        assert_eq!(counts, BlockCounts { oodle: 1, lzf: 1, stored: 1 });
    }

    #[test]
    fn entry_without_a_header_is_one_block() {
        let (out, counts) = decompress(&LZF, LZF_OUT.len(), None).unwrap();
        assert_eq!(out, LZF_OUT);
        assert_eq!(counts, BlockCounts { lzf: 1, ..Default::default() });
        let (out, _) = decompress(&flip(b"bare oodle"), 10, Some(&Flip)).unwrap();
        assert_eq!(out, b"bare oodle");
    }

    #[test]
    fn stored_block_starting_with_the_oodle_byte() {
        let payload = [OODLE_MAGIC, 1, 2, 3];
        let mut entry = block(&payload, 4);
        entry.extend(block(&LZF, LZF_OUT.len()));
        let (out, counts) = decompress(&entry, 4 + LZF_OUT.len(), Some(&Flip)).unwrap();
        assert_eq!(&out[..4], payload);
        assert_eq!(counts, BlockCounts { oodle: 0, lzf: 1, stored: 1 });
    }

    #[test]
    fn oodle_without_a_library() {
        let entry = block(&flip(b"oodle"), 5);
        assert_eq!(decompress(&entry, 5, None), Err(BlockError::NoOodle { offset: 0 }));
    }

    #[test]
    fn errors_name_the_block() {
        let first = block(&LZF, LZF_OUT.len());
        let n = LZF_OUT.len();

        let mut no_second_header = first.clone();
        no_second_header.extend_from_slice(&[0u8; 16]);
        assert_eq!(decompress(&no_second_header, n + 5, None), Err(BlockError::NoHeader { offset: first.len() }));

        let mut trailing = first.clone();
        trailing.extend_from_slice(b"junk");
        assert_eq!(decompress(&trailing, n, None), Err(BlockError::Trailing { left: 4 }));

        assert_eq!(decompress(&first, n - 1, None), Err(BlockError::PastOutput { offset: 0, size: n }));
        assert_eq!(decompress(&first[..first.len() - 1], n, None), Err(BlockError::PastInput { offset: 0, compressed: LZF.len() }));
        assert_eq!(decompress(&header(0, 4), 4, None), Err(BlockError::Empty { offset: 0 }));

        let bad_lzf = block(&[1 << 5, 0, 0], 9);
        assert!(matches!(decompress(&bad_lzf, 9, None), Err(BlockError::Lzf { offset: 0, source: LzfError::BadDistance })));
        let bad_oodle = block(&[OODLE_MAGIC, 1, 2], 9);
        assert!(matches!(decompress(&bad_oodle, 9, Some(&Flip)), Err(BlockError::Oodle { offset: 0, .. })));
    }
}
