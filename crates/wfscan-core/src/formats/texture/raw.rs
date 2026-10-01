//! The texture header: what follows the common header in a texture's H part.
//!
//! Layout, little-endian:
//! - u8: mip levels stored in the B part (the smallest ones);
//! - u8: mip levels stored in the F part (the largest ones);
//! - u8: unknown;
//! - u8: pixel format code;
//! - u32 count, then that many u32s (one fewer than the F mips: probably where each one
//!   starts in the compressed F data, for streaming; not needed to read the file);
//! - u16 width, u16 height: the size the texture was authored at;
//! - u16 width, u16 height of the largest mip in B (0, 0 if B holds none);
//! - u16: the larger of the width and height actually stored. The game ships many
//!   textures at half or a quarter of their authored size;
//! - more fields (flags, floats, source paths) that the size and layout don't depend on.
//!
//! Sources: the format byte, the count-and-skip and the authored size come from the
//! original's texture pattern. The mip counts, the B size and the stored size were worked
//! out from the September 2026 cache, where they predict the B and F sizes of all
//! 128,668 textures in formats wfscan knows.

/// More entries than any texture has mips.
const MAX_OFFSETS: u32 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureHeader {
    pub b_mips: u8,
    pub f_mips: u8,
    pub unknown: u8,
    /// See [`super::model::PixelFormat::from_code`].
    pub format: u8,
    pub f_offsets: Vec<u32>,
    /// Authored size.
    pub width: u16,
    pub height: u16,
    /// Size of the largest mip in B.
    pub b_width: u16,
    pub b_height: u16,
    /// The larger stored dimension of the top mip.
    pub stored_size: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    #[error("the texture header is cut short")]
    Truncated,
    #[error("the texture header lists {0} mip offsets")]
    TooManyOffsets(u32),
}

/// Parse the texture header from an H part's bytes after the common header.
pub fn parse(data: &[u8]) -> Result<TextureHeader, HeaderError> {
    let fixed: &[u8; 8] = data.first_chunk().ok_or(HeaderError::Truncated)?;
    let count = u32::from_le_bytes([fixed[4], fixed[5], fixed[6], fixed[7]]);
    if count > MAX_OFFSETS {
        return Err(HeaderError::TooManyOffsets(count));
    }
    let rest = &data[8..];
    let (offsets, rest) = rest.split_at_checked(4 * count as usize).ok_or(HeaderError::Truncated)?;
    let sizes: &[u8; 10] = rest.first_chunk().ok_or(HeaderError::Truncated)?;
    let u16_at = |i: usize| u16::from_le_bytes([sizes[i], sizes[i + 1]]);
    Ok(TextureHeader {
        b_mips: fixed[0],
        f_mips: fixed[1],
        unknown: fixed[2],
        format: fixed[3],
        f_offsets: offsets.as_chunks::<4>().0.iter().map(|o| u32::from_le_bytes(*o)).collect(),
        width: u16_at(0),
        height: u16_at(2),
        b_width: u16_at(4),
        b_height: u16_at(6),
        stored_size: u16_at(8),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_header() {
        // /EE/Characters/BoxMan/BoxMan_d.png: 1024x1024, 9 mips in B from 256x256, 2 in F.
        let bytes = [0x09, 0x02, 0x01, 0x00, 0x01, 0, 0, 0, 0x19, 0xe4, 0, 0, 0x00, 0x04, 0x00, 0x04, 0x00, 0x01, 0x00, 0x01, 0x00, 0x04, 0, 0, 1, 1];
        let h = parse(&bytes).unwrap();
        assert_eq!((h.b_mips, h.f_mips, h.unknown, h.format), (9, 2, 1, 0));
        assert_eq!(h.f_offsets, [0xe419]);
        assert_eq!((h.width, h.height, h.b_width, h.b_height, h.stored_size), (1024, 1024, 256, 256, 1024));
        for cut in 0..22 {
            assert_eq!(parse(&bytes[..cut]), Err(HeaderError::Truncated), "cut at {cut}");
        }
        assert!(parse(&bytes[..22]).is_ok());
    }

    #[test]
    fn absurd_counts_are_refused() {
        let mut bytes = vec![1, 0, 1, 10];
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(&[0; 64]);
        assert_eq!(parse(&bytes), Err(HeaderError::TooManyOffsets(u32::MAX)));
    }
}
