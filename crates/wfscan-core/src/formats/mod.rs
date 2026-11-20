//! One folder per kind of asset. Each keeps the same three steps: `raw` reads the file
//! into a struct that mirrors it, `model` turns that into something free of the game's
//! storage quirks, and a writer (`dds`, ...) produces a standard format.

pub mod audio;
pub mod model;
pub mod passthrough;
pub mod reader;
pub mod texture;
pub mod video;

use crate::export::Exporters;

/// Choices the exporters offer, and what they need from outside the cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormatOptions {
    pub texture: texture::TextureOutput,
    pub audio: audio::AudioOutput,
    /// Vorbis header sets to use instead of the bundled ones.
    pub vorbis: Option<std::sync::Arc<audio::VorbisTables>>,
    pub video: video::VideoOutput,
    /// The user's Bink library, for videos as PNG frames.
    pub bink: Option<std::sync::Arc<video::BinkLibrary>>,
}

impl Exporters {
    /// The exporters wfscan ships.
    pub fn builtin(options: &FormatOptions) -> Self {
        let exporters = Self::default()
            .with(texture::TextureExporter { output: options.texture })
            .with(model::ModelExporter)
            .with(audio::AudioExporter { output: options.audio, vorbis: options.vorbis.clone() })
            .with(video::VideoExporter { output: options.video, bink: options.bink.clone() });
        passthrough::PassthroughExporter::KINDS.into_iter().fold(exporters, |all, kind| all.with(passthrough::PassthroughExporter { kind }))
    }
}
