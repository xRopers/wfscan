//! Writing a [`Texture`] as a DirectDraw Surface file.
//!
//! Always with the DX10 header: it names the pixel format exactly (BC6H, BC7 and arrays
//! have no older spelling) and every current tool reads it.
//!
//! Source: Microsoft's DDS file format documentation (`DDS_HEADER`, `DDS_HEADER_DXT10`).

use super::model::{Shape, Texture};

const DDSD_CAPS: u32 = 0x1;
const DDSD_HEIGHT: u32 = 0x2;
const DDSD_WIDTH: u32 = 0x4;
const DDSD_PITCH: u32 = 0x8;
const DDSD_PIXELFORMAT: u32 = 0x1000;
const DDSD_MIPMAPCOUNT: u32 = 0x2_0000;
const DDSD_LINEARSIZE: u32 = 0x8_0000;
const DDSD_DEPTH: u32 = 0x80_0000;

const DDPF_FOURCC: u32 = 0x4;

const DDSCAPS_COMPLEX: u32 = 0x8;
const DDSCAPS_TEXTURE: u32 = 0x1000;
const DDSCAPS_MIPMAP: u32 = 0x40_0000;

const DDSCAPS2_CUBEMAP_ALL_FACES: u32 = 0x200 | 0xfc00;
const DDSCAPS2_VOLUME: u32 = 0x20_0000;

const DIMENSION_TEXTURE2D: u32 = 3;
const DIMENSION_TEXTURE3D: u32 = 4;
const MISC_TEXTURECUBE: u32 = 0x4;

/// Magic, `DDS_HEADER` and `DDS_HEADER_DXT10`.
pub const HEADER_LEN: usize = 4 + 124 + 20;

/// The whole `.dds` file for `texture`.
pub fn write(texture: &Texture) -> Vec<u8> {
    let volume_depth = match texture.shape {
        Shape::Volume(depth) => Some(depth),
        _ => None,
    };
    let (size_flag, size) = match texture.format.block_bytes() {
        Some(_) => (DDSD_LINEARSIZE, texture.format.image_size(texture.width, texture.height) as u32),
        None => (DDSD_PITCH, texture.format.image_size(texture.width, 1) as u32),
    };
    let mut flags = DDSD_CAPS | DDSD_HEIGHT | DDSD_WIDTH | DDSD_PIXELFORMAT | size_flag;
    let mut caps = DDSCAPS_TEXTURE;
    if texture.mips > 1 {
        flags |= DDSD_MIPMAPCOUNT;
        caps |= DDSCAPS_MIPMAP | DDSCAPS_COMPLEX;
    }
    if volume_depth.is_some() {
        flags |= DDSD_DEPTH;
    }
    if texture.shape != Shape::Flat {
        caps |= DDSCAPS_COMPLEX;
    }
    let caps2 = match texture.shape {
        Shape::Cube => DDSCAPS2_CUBEMAP_ALL_FACES,
        Shape::Volume(_) => DDSCAPS2_VOLUME,
        Shape::Flat | Shape::Array(_) => 0,
    };

    let mut out = Vec::with_capacity(HEADER_LEN + texture.data.len());
    let mut put = |value: u32| out.extend_from_slice(&value.to_le_bytes());
    put(u32::from_le_bytes(*b"DDS "));
    put(124);
    put(flags);
    put(texture.height);
    put(texture.width);
    put(size);
    put(volume_depth.unwrap_or(0));
    put(texture.mips);
    (0..11).for_each(|_| put(0)); // reserved
    put(32); // pixel format: only says "see the DX10 header"
    put(DDPF_FOURCC);
    put(u32::from_le_bytes(*b"DX10"));
    (0..5).for_each(|_| put(0)); // bit count and masks
    put(caps);
    put(caps2);
    (0..3).for_each(|_| put(0)); // caps3, caps4, reserved

    put(texture.format.dxgi());
    put(if volume_depth.is_some() { DIMENSION_TEXTURE3D } else { DIMENSION_TEXTURE2D });
    put(if texture.shape == Shape::Cube { MISC_TEXTURECUBE } else { 0 });
    put(match texture.shape {
        Shape::Array(layers) => layers,
        _ => 1, // a cube map counts as one element of six faces
    });
    put(0); // alpha mode unknown
    debug_assert_eq!(out.len(), HEADER_LEN);
    out.extend_from_slice(&texture.data);
    out
}

#[cfg(test)]
mod tests {
    use super::super::model::PixelFormat;
    use super::*;

    fn texture(format: PixelFormat, width: u32, height: u32, mips: u32, shape: Shape) -> Texture {
        let per_element: usize = (0..mips).map(|m| format.image_size((width >> m).max(1), (height >> m).max(1))).sum();
        Texture { format, width, height, mips, shape, data: vec![0xab; per_element * shape.elements() as usize] }
    }

    fn field(dds: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(dds[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn header_fields() {
        let t = texture(PixelFormat::Bc7, 256, 128, 9, Shape::Flat);
        let dds = write(&t);
        assert_eq!(&dds[..4], b"DDS ");
        assert_eq!(dds.len(), HEADER_LEN + t.data.len());
        assert_eq!((field(&dds, 4), field(&dds, 12), field(&dds, 16), field(&dds, 28)), (124, 128, 256, 9));
        assert_eq!(field(&dds, 20), 64 * 32 * 16, "linear size of the top mip");
        assert_eq!((field(&dds, 76), field(&dds, 80), &dds[84..88]), (32, DDPF_FOURCC, &b"DX10"[..]));
        assert_eq!((field(&dds, 128), field(&dds, 132), field(&dds, 136), field(&dds, 140)), (98, DIMENSION_TEXTURE2D, 0, 1));
        assert_eq!(field(&dds, 8) & (DDSD_MIPMAPCOUNT | DDSD_LINEARSIZE | DDSD_PITCH | DDSD_DEPTH), DDSD_MIPMAPCOUNT | DDSD_LINEARSIZE);
    }

    #[test]
    fn shapes() {
        let cube = write(&texture(PixelFormat::Bc6h, 8, 8, 4, Shape::Cube));
        assert_eq!((field(&cube, 112), field(&cube, 136), field(&cube, 140)), (DDSCAPS2_CUBEMAP_ALL_FACES, MISC_TEXTURECUBE, 1));
        let array = write(&texture(PixelFormat::Bc3, 16, 16, 5, Shape::Array(5)));
        assert_eq!((field(&array, 112), field(&array, 136), field(&array, 140)), (0, 0, 5));
        let volume = write(&texture(PixelFormat::Bgra8, 16, 16, 1, Shape::Volume(16)));
        assert_eq!((field(&volume, 24), field(&volume, 112), field(&volume, 132)), (16, DDSCAPS2_VOLUME, DIMENSION_TEXTURE3D));
        assert_eq!(field(&volume, 20), 64, "pitch of one row");
        assert_eq!(field(&volume, 8) & (DDSD_MIPMAPCOUNT | DDSD_PITCH | DDSD_DEPTH), DDSD_PITCH | DDSD_DEPTH);
    }

    /// texscan's reader, written independently, sees what was meant.
    #[cfg(feature = "png")]
    #[test]
    fn texscan_reads_it_back() {
        let cases = [
            (PixelFormat::Bc1, 1024, 1024, 11, Shape::Flat, "BC1_UNORM"),
            (PixelFormat::Bc5, 512, 1024, 11, Shape::Flat, "BC5_UNORM"),
            (PixelFormat::Bc6h, 128, 128, 8, Shape::Cube, "BC6H_UF16"),
            (PixelFormat::Bc3, 512, 512, 10, Shape::Array(5), "BC3_UNORM"),
            (PixelFormat::Bgra8, 33, 33, 1, Shape::Volume(33), "B8G8R8A8_UNORM"),
            (PixelFormat::A8, 128, 128, 8, Shape::Flat, "A8_UNORM"),
            (PixelFormat::R16, 64, 64, 1, Shape::Flat, "R16_UNORM"),
            (PixelFormat::Bc7, 4, 4, 3, Shape::Flat, "BC7_UNORM"),
            (PixelFormat::Bc2, 16, 16, 1, Shape::Flat, "BC2_UNORM"),
            (PixelFormat::Bc4, 16, 8, 5, Shape::Flat, "BC4_UNORM"),
        ];
        for (format, width, height, mips, shape, name) in cases {
            let dds = write(&texture(format, width, height, mips, shape));
            let info = texscan_core::formats::dds::parse(&dds).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(info.size, dds.len() as u64, "{name}: every byte is accounted for");
            assert_eq!((info.width, info.height, info.mips, info.pixel_format.name), (width, height, mips, name));
            let (faces, array, depth) = match shape {
                Shape::Flat => (1, 1, 1),
                Shape::Cube => (6, 1, 1),
                Shape::Array(n) => (1, n, 1),
                Shape::Volume(n) => (1, 1, n),
            };
            assert_eq!((info.faces, info.array_size, info.depth), (faces, array, depth), "{name}");
        }
    }
}
