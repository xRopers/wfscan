//! One folder per kind of asset. Each keeps the same three steps: `raw` reads the file
//! into a struct that mirrors it, `model` turns that into something free of the game's
//! storage quirks, and a writer (`dds`, ...) produces a standard format.

pub mod audio;
pub mod model;
pub mod reader;
pub mod texture;

use crate::export::Exporters;

/// Choices the exporters offer, and what they need from outside the cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormatOptions {
    pub texture: texture::TextureOutput,
    pub audio: audio::AudioOutput,
    /// Vorbis header sets to use instead of the bundled ones.
    pub vorbis: Option<std::sync::Arc<audio::VorbisTables>>,
}

impl Exporters {
    /// The exporters wfscan ships.
    pub fn builtin(options: &FormatOptions) -> Self {
        Self::default()
            .with(texture::TextureExporter { output: options.texture })
            .with(model::ModelExporter)
            .with(audio::AudioExporter { output: options.audio, vorbis: options.vorbis.clone() })
    }
}
