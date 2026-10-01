//! One folder per kind of asset. Each keeps the same three steps: `raw` reads the file
//! into a struct that mirrors it, `model` turns that into something free of the game's
//! storage quirks, and a writer (`dds`, ...) produces a standard format.

pub mod texture;

use crate::export::Exporters;

/// Choices the exporters offer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FormatOptions {
    pub texture: texture::TextureOutput,
}

impl Exporters {
    /// The exporters wfscan ships.
    pub fn builtin(options: &FormatOptions) -> Self {
        Self::default().with(texture::TextureExporter { output: options.texture })
    }
}
