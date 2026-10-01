//! Textures: the `Texture` and `LightMap` packages.
//!
//! DDS output is exact: the game's own compressed data with a standard header, every mip
//! and every face, layer or slice. PNG output is the top mip decoded to 8-bit RGBA, one
//! file per face, layer or slice; HDR data (BC6H) is clamped to 0-1, so use DDS for those.

pub mod dds;
pub mod model;
pub mod raw;

use std::fmt;
use std::str::FromStr;

use crate::cache::package::{Category, Part};
use crate::export::{ExportError, ExportInput, Exporter, Output};
use crate::types::Kind;

pub use model::{PixelFormat, Shape, Texture, TextureError};
pub use raw::TextureHeader;

/// What a texture is exported as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextureOutput {
    #[default]
    Dds,
    Png,
}

impl TextureOutput {
    pub fn name(self) -> &'static str {
        match self {
            Self::Dds => "dds",
            Self::Png => "png",
        }
    }
}

impl fmt::Display for TextureOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for TextureOutput {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "dds" => Ok(Self::Dds),
            "png" => Ok(Self::Png),
            _ => Err(format!("unknown texture format {s:?} (dds or png)")),
        }
    }
}

/// Whether a file type holds cube maps, so that six elements are six faces.
/// Source: the original's type names (`TEXTURE_CUBEMAP`, `LIGHTMAP_XYZ`); 183 from the
/// September 2026 cache, where all 12,404 files have six elements.
fn is_cube_type(category: Category, type_id: u32) -> bool {
    matches!((category, type_id), (Category::Texture, 177) | (Category::LightMap, 177 | 183))
}

/// File extensions the game's texture paths end in, which the output's replaces.
const SOURCE_EXTENSIONS: [&str; 12] = ["png", "tga", "hdr", "raw", "exr", "cube", "jpg", "bmp", "tif", "dds", "lcsm", "lchm"];

/// `/A/B_d.png` as `/A/B_d<suffix>.<extension>`; a path without a known image extension
/// keeps its whole name.
pub fn output_path(path: &str, suffix: &str, extension: &str) -> String {
    let stem = match path.rsplit_once('.') {
        Some((stem, ext)) if !ext.contains('/') && SOURCE_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext)) => stem,
        _ => path,
    };
    format!("{stem}{suffix}.{extension}")
}

/// Read one texture: its header from H, its mips from B and F.
pub fn read(input: &ExportInput) -> Result<Texture, ExportError> {
    let header = raw::parse(input.h_body()).map_err(|e| ExportError::failed(e.to_string()))?;
    if PixelFormat::from_code(header.format).is_none() {
        return Err(ExportError::unsupported(TextureError::UnknownFormat(header.format).to_string()));
    }
    let part = |part: Part| if input.has(part) { input.read(part) } else { Ok(Vec::new()) };
    let (b, f) = (part(Part::B)?, part(Part::F)?);
    Texture::assemble(&header, is_cube_type(input.category, input.type_id()), &b, &f).map_err(|e| ExportError::failed(e.to_string()))
}

pub struct TextureExporter {
    pub output: TextureOutput,
}

impl Exporter for TextureExporter {
    /// The output format is part of the name, so switching formats exports again.
    fn name(&self) -> &'static str {
        match self.output {
            TextureOutput::Dds => "texture-dds",
            TextureOutput::Png => "texture-png",
        }
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> Kind {
        Kind::Texture
    }

    fn export(&self, input: &ExportInput) -> Result<Vec<Output>, ExportError> {
        let texture = read(input)?;
        let file = dds::write(&texture);
        match self.output {
            TextureOutput::Dds => Ok(vec![Output { path: output_path(input.path, "", "dds"), data: file }]),
            TextureOutput::Png => pngs(&texture, &file, input.path),
        }
    }
}

/// The top mip of every face, layer and slice as a PNG, decoded by texscan from the DDS.
#[cfg(feature = "png")]
fn pngs(texture: &Texture, dds: &[u8], path: &str) -> Result<Vec<Output>, ExportError> {
    let info = texscan_core::formats::dds::parse(dds).map_err(|e| ExportError::failed(format!("the DDS written doesn't parse: {e:?}")))?;
    let mut outputs = Vec::new();
    for (image, suffix) in texscan_core::png_images(&info) {
        let mut decoded = texscan_core::decode(dds, &info, image).map_err(|e| ExportError::failed(format!("decoding {:?}: {e}", texture.format)))?;
        if texture.format == PixelFormat::A8 {
            // The one channel is the picture: show it as grey, not as the alpha of black.
            for pixel in decoded.rgba.as_chunks_mut::<4>().0 {
                *pixel = [pixel[3], pixel[3], pixel[3], 255];
            }
        }
        outputs.push(Output { path: output_path(path, &suffix, "png"), data: texscan_core::encode_png(&decoded) });
    }
    Ok(outputs)
}

/// One decoded image of a texture: 8-bit RGBA, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Decode one element (face, layer or slice) of `texture` for display: the largest mip
/// no wider or taller than `max_size`, or the smallest there is.
#[cfg(feature = "png")]
pub fn decode_rgba(texture: &Texture, element: u32, max_size: u32) -> Result<Rgba, String> {
    let file = dds::write(texture);
    let info = texscan_core::formats::dds::parse(&file).map_err(|e| format!("the DDS written doesn't parse: {e:?}"))?;
    let fits = |mip: &u32| texture.mip_size(*mip).0.max(texture.mip_size(*mip).1) <= max_size;
    let mip = (0..texture.mips).find(fits).unwrap_or(texture.mips - 1);
    let image = match texture.shape {
        Shape::Volume(_) => texscan_core::Subresource { layer: 0, mip, slice: element },
        _ => texscan_core::Subresource { layer: element, mip, slice: 0 },
    };
    let mut decoded = texscan_core::decode(&file, &info, image).map_err(|e| e.to_string())?;
    if texture.format == PixelFormat::A8 {
        for pixel in decoded.rgba.as_chunks_mut::<4>().0 {
            *pixel = [pixel[3], pixel[3], pixel[3], 255];
        }
    }
    Ok(Rgba { width: decoded.width, height: decoded.height, pixels: decoded.rgba })
}

#[cfg(not(feature = "png"))]
pub fn decode_rgba(_texture: &Texture, _element: u32, _max_size: u32) -> Result<Rgba, String> {
    Err("this wfscan was built without texture decoding (rebuild with the `png` feature)".into())
}

#[cfg(not(feature = "png"))]
fn pngs(_texture: &Texture, _dds: &[u8], _path: &str) -> Result<Vec<Output>, ExportError> {
    Err(ExportError::unsupported("this wfscan was built without PNG support (rebuild with the `png` feature)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_paths() {
        assert_eq!(output_path("/A/B_d.png", "", "dds"), "/A/B_d.dds");
        assert_eq!(output_path("/A/B_d.PNG", "_px", "png"), "/A/B_d_px.png");
        assert_eq!(output_path("/A/BPackMap", "", "dds"), "/A/BPackMap.dds");
        assert_eq!(output_path("/A.b/C", "", "dds"), "/A.b/C.dds");
        assert_eq!(output_path("/A/0_cross.hdr", "_nz", "png"), "/A/0_cross_nz.png");
        assert_eq!(output_path("/A/Thing.v2", "", "dds"), "/A/Thing.v2.dds", "not an image extension");
    }

    #[test]
    fn cube_types() {
        assert!(is_cube_type(Category::Texture, 177) && is_cube_type(Category::LightMap, 183));
        assert!(!is_cube_type(Category::Texture, 183) && !is_cube_type(Category::Misc, 177));
    }

    #[test]
    fn output_names() {
        assert_eq!("png".parse::<TextureOutput>(), Ok(TextureOutput::Png));
        assert_eq!(TextureOutput::default().to_string(), "dds");
        assert!("jpg".parse::<TextureOutput>().is_err());
    }
}
