//! A texture as a standard tool sees it: a pixel format, a size, a mip chain and a shape,
//! with the images in DDS order.
//!
//! How the game stores them:
//! - The mip chain is split in two. B holds the smallest mips, F the largest; either can
//!   be absent (icons keep everything in F, small textures everything in B).
//! - Within each part the mips run from smallest to largest, and each mip holds all of
//!   the texture's elements (cube faces, array layers, volume slices) one after another.
//! - Nothing says how many elements there are; it is the data size divided by the size
//!   of one element's mip chain. Six of them in a cube map type are faces; several with
//!   a single mip are the slices of a volume; otherwise they are array layers.
//!
//! Sources: the smallest-first order and elements-within-a-mip follow from where the
//! original takes the top mip (the end of the data, one per element). The split and the
//! element count were worked out from the September 2026 cache: the sizes fit all
//! 128,668 textures in known formats, and neighbouring mips of the result agree with
//! each other (`tests/textures.rs`).

use super::raw::TextureHeader;

/// The pixel formats the game uses that wfscan knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bc1,
    Bc2,
    Bc3,
    Bc4,
    Bc5,
    Bc6h,
    Bc7,
    Bgra8,
    A8,
    R16,
}

impl PixelFormat {
    /// The format for a header's format code. 31, 32, 33 and 44 also occur and are not
    /// known. Source: the original's format table.
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 | 1 => Self::Bc1,
            2 => Self::Bc2,
            3 => Self::Bc3,
            6 => Self::Bc4,
            7 => Self::Bc5,
            10 => Self::Bgra8,
            12 => Self::A8,
            30 => Self::R16,
            34 => Self::Bc7,
            35 => Self::Bc6h,
            _ => return None,
        })
    }

    /// The DXGI format number.
    pub fn dxgi(self) -> u32 {
        match self {
            Self::Bc1 => 71,
            Self::Bc2 => 74,
            Self::Bc3 => 77,
            Self::Bc4 => 80,
            Self::Bc5 => 83,
            Self::Bc6h => 95,
            Self::Bc7 => 98,
            Self::Bgra8 => 87,
            Self::A8 => 65,
            Self::R16 => 56,
        }
    }

    /// Bytes per 4x4 block, for the block-compressed formats.
    pub fn block_bytes(self) -> Option<usize> {
        match self {
            Self::Bc1 | Self::Bc4 => Some(8),
            Self::Bc2 | Self::Bc3 | Self::Bc5 | Self::Bc6h | Self::Bc7 => Some(16),
            Self::Bgra8 | Self::A8 | Self::R16 => None,
        }
    }

    /// Bytes per pixel, for the uncompressed formats.
    pub fn pixel_bytes(self) -> Option<usize> {
        match self {
            Self::Bgra8 => Some(4),
            Self::R16 => Some(2),
            Self::A8 => Some(1),
            _ => None,
        }
    }

    /// Bytes in one `width` x `height` image.
    pub fn image_size(self, width: u32, height: u32) -> usize {
        let (w, h) = (width.max(1) as usize, height.max(1) as usize);
        match (self.block_bytes(), self.pixel_bytes()) {
            (Some(block), _) => w.div_ceil(4) * h.div_ceil(4) * block,
            (None, Some(pixel)) => w * h * pixel,
            (None, None) => unreachable!("every format is block-compressed or not"),
        }
    }
}

/// What the elements of a texture are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// One image per mip.
    Flat,
    /// Six faces per mip.
    Cube,
    /// This many layers per mip.
    Array(u32),
    /// A single mip of this many slices.
    Volume(u32),
}

impl Shape {
    pub fn elements(self) -> u32 {
        match self {
            Shape::Flat => 1,
            Shape::Cube => 6,
            Shape::Array(n) | Shape::Volume(n) => n,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texture {
    pub format: PixelFormat,
    /// Size of the top mip as stored, which may be smaller than the authored size.
    pub width: u32,
    pub height: u32,
    pub mips: u32,
    pub shape: Shape,
    /// The images in DDS order: element by element, each with its mips from largest to
    /// smallest. A volume is its slices in order.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextureError {
    #[error("pixel format {0} is not known")]
    UnknownFormat(u8),
    #[error("the header gives no size or no mips")]
    Empty,
    #[error("B's largest mip should be {expected_width}x{expected_height}, the header says {width}x{height}")]
    BSize { expected_width: u32, expected_height: u32, width: u32, height: u32 },
    #[error(
        "{width}x{height} {format:?} with {b_mips}+{f_mips} mips needs {b_chain} bytes in B and {f_chain} in F per element; \
         B has {b_len} and F has {f_len}"
    )]
    DataSize { format: PixelFormat, width: u32, height: u32, b_mips: u8, f_mips: u8, b_chain: usize, f_chain: usize, b_len: usize, f_len: usize },
}

impl Texture {
    pub fn mip_size(&self, mip: u32) -> (u32, u32) {
        ((self.width >> mip).max(1), (self.height >> mip).max(1))
    }

    /// Put a texture together from its header and its B and F data (empty if absent).
    /// `cube_type` says the file type is one that holds cube maps.
    pub fn assemble(header: &TextureHeader, cube_type: bool, b: &[u8], f: &[u8]) -> Result<Texture, TextureError> {
        let format = PixelFormat::from_code(header.format).ok_or(TextureError::UnknownFormat(header.format))?;
        let (authored_w, authored_h, stored) = (u32::from(header.width), u32::from(header.height), u32::from(header.stored_size));
        let (b_mips, f_mips) = (u32::from(header.b_mips), u32::from(header.f_mips));
        let mips = b_mips + f_mips;
        let larger = authored_w.max(authored_h);
        if larger == 0 || stored == 0 || mips == 0 {
            return Err(TextureError::Empty);
        }
        // The stored size keeps the authored proportions.
        let width = (authored_w * stored / larger).max(1);
        let height = (authored_h * stored / larger).max(1);
        let size = |mip: u32| ((width >> mip).max(1), (height >> mip).max(1));
        if b_mips > 0 {
            let (expected_width, expected_height) = size(f_mips);
            let (bw, bh) = (u32::from(header.b_width), u32::from(header.b_height));
            if (bw, bh) != (expected_width, expected_height) {
                return Err(TextureError::BSize { expected_width, expected_height, width: bw, height: bh });
            }
        }

        let image = |mip: u32| {
            let (w, h) = size(mip);
            format.image_size(w, h)
        };
        let f_chain: usize = (0..f_mips).map(image).sum();
        let b_chain: usize = (f_mips..mips).map(image).sum();
        let elements = (b.len() + f.len()) / (b_chain + f_chain);
        if elements == 0 || b.len() != elements * b_chain || f.len() != elements * f_chain || u32::try_from(elements).is_err() {
            return Err(TextureError::DataSize {
                format,
                width,
                height,
                b_mips: header.b_mips,
                f_mips: header.f_mips,
                b_chain,
                f_chain,
                b_len: b.len(),
                f_len: f.len(),
            });
        }
        let shape = match elements as u32 {
            1 => Shape::Flat,
            6 if cube_type => Shape::Cube,
            n if mips == 1 => Shape::Volume(n),
            n => Shape::Array(n),
        };

        // Where each mip's block of elements starts: smallest first within its part.
        let mip_data = |mip: u32| {
            let (part, last) = if mip < f_mips { (f, f_mips) } else { (b, mips) };
            let start: usize = (mip + 1..last).map(image).sum::<usize>() * elements;
            &part[start..start + image(mip) * elements]
        };
        let mut data = Vec::with_capacity(b.len() + f.len());
        for element in 0..elements {
            for mip in 0..mips {
                let one = image(mip);
                data.extend_from_slice(&mip_data(mip)[element * one..(element + 1) * one]);
            }
        }
        Ok(Texture { format, width, height, mips, shape, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(format: u8, b_mips: u8, f_mips: u8, authored: (u16, u16), b_size: (u16, u16), stored: u16) -> TextureHeader {
        TextureHeader {
            b_mips,
            f_mips,
            unknown: 1,
            format,
            f_offsets: vec![],
            width: authored.0,
            height: authored.1,
            b_width: b_size.0,
            b_height: b_size.1,
            stored_size: stored,
        }
    }

    /// Bytes numbered so every image is recognisable: `tag` in the high nibble.
    fn image(tag: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| tag << 4 | (i % 16) as u8).collect()
    }

    #[test]
    fn image_sizes() {
        assert_eq!(PixelFormat::Bc1.image_size(1024, 1024), 524_288);
        assert_eq!(PixelFormat::Bc1.image_size(1, 1), 8);
        assert_eq!(PixelFormat::Bc7.image_size(5, 3), 32);
        assert_eq!(PixelFormat::Bgra8.image_size(3, 2), 24);
        assert_eq!(PixelFormat::R16.image_size(4, 4), 32);
        assert_eq!(PixelFormat::A8.image_size(0, 7), 7);
        assert_eq!(PixelFormat::from_code(31), None);
    }

    #[test]
    fn mips_are_put_largest_first() {
        // 8x8 BGRA: 4x4, 2x2 and 1x1 in B (smallest first), 8x8 in F.
        let h = header(10, 3, 1, (8, 8), (4, 4), 8);
        let b = [image(3, 4), image(2, 16), image(1, 64)].concat();
        let f = image(0, 256);
        let t = Texture::assemble(&h, false, &b, &f).unwrap();
        assert_eq!((t.format, t.width, t.height, t.mips, t.shape), (PixelFormat::Bgra8, 8, 8, 4, Shape::Flat));
        assert_eq!(t.data, [image(0, 256), image(1, 64), image(2, 16), image(3, 4)].concat());
        assert_eq!(t.mip_size(3), (1, 1));
    }

    #[test]
    fn everything_in_one_part() {
        let all_b = header(10, 2, 0, (2, 2), (2, 2), 2);
        let b = [image(1, 4), image(0, 16)].concat();
        assert_eq!(Texture::assemble(&all_b, false, &b, &[]).unwrap().data, [image(0, 16), image(1, 4)].concat());

        // Icons: nothing in B, whose size is then 0x0.
        let all_f = header(10, 0, 2, (2, 2), (0, 0), 2);
        assert_eq!(Texture::assemble(&all_f, false, &[], &b).unwrap().data, [image(0, 16), image(1, 4)].concat());
    }

    #[test]
    fn stored_smaller_than_authored_and_not_square() {
        // Authored 2048x4096, stored 512x1024: BC1, 256x512 and one smaller mip in F...
        let h = header(1, 9, 2, (2048, 4096), (128, 256), 1024);
        let t = Texture::assemble(&h, false, &vec![0; 21_864], &vec![0; 327_680]).unwrap();
        assert_eq!((t.width, t.height, t.mips), (512, 1024, 11));
        assert_eq!(t.mip_size(10), (1, 1));
    }

    #[test]
    fn elements_come_from_the_data_size() {
        // Two layers of 2x2 + 1x1: each mip holds both layers.
        let h = header(10, 1, 1, (2, 2), (1, 1), 2);
        let b = [image(1, 4), image(3, 4)].concat();
        let f = [image(0, 16), image(2, 16)].concat();
        let t = Texture::assemble(&h, false, &b, &f).unwrap();
        assert_eq!(t.shape, Shape::Array(2));
        assert_eq!(t.data, [image(0, 16), image(1, 4), image(2, 16), image(3, 4)].concat());

        // Six elements are faces only in a cube map type.
        let one_mip = header(10, 1, 0, (1, 1), (1, 1), 1);
        let six: Vec<u8> = (0..6).flat_map(|i| image(i, 4)).collect();
        assert_eq!(Texture::assemble(&one_mip, true, &six, &[]).unwrap().shape, Shape::Cube);
        let volume = Texture::assemble(&one_mip, false, &six, &[]).unwrap();
        assert_eq!((volume.shape, &volume.data), (Shape::Volume(6), &six));
        assert_eq!(Texture::assemble(&one_mip, true, &six[..20], &[]).unwrap().shape, Shape::Volume(5));
    }

    #[test]
    fn what_does_not_fit_is_refused() {
        let h = header(10, 3, 1, (8, 8), (4, 4), 8);
        let (b, f) = (vec![0; 84], vec![0; 256]);
        assert!(Texture::assemble(&h, false, &b, &f).is_ok());
        let size_error = |b: &[u8], f: &[u8]| matches!(Texture::assemble(&h, false, b, f), Err(TextureError::DataSize { b_chain: 84, f_chain: 256, .. }));
        assert!(size_error(&b[..83], &f));
        assert!(size_error(&b, &[]), "F missing");
        assert!(size_error(&[b.clone(), b.clone()].concat(), &f), "two elements in B, one in F");
        assert!(size_error(&[], &[]));

        assert_eq!(Texture::assemble(&header(31, 3, 1, (8, 8), (4, 4), 8), false, &b, &f), Err(TextureError::UnknownFormat(31)));
        assert_eq!(Texture::assemble(&header(10, 0, 0, (8, 8), (0, 0), 8), false, &b, &f), Err(TextureError::Empty));
        assert_eq!(Texture::assemble(&header(10, 3, 1, (0, 0), (4, 4), 8), false, &b, &f), Err(TextureError::Empty));
        assert!(matches!(
            Texture::assemble(&header(10, 3, 1, (8, 8), (2, 2), 8), false, &b, &f),
            Err(TextureError::BSize { expected_width: 4, expected_height: 4, width: 2, height: 2 })
        ));
    }
}
