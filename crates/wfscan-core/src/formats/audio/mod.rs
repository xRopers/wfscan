//! Sounds: type 139 in the `Misc` packages, as PCM, Opus or Vorbis.
//!
//! Exported in their own format by default, with nothing re-encoded: Vorbis as Ogg
//! Vorbis (`.ogg`), Opus as Ogg Opus (`.opus`), PCM as `.wav`. Or all decoded to 16-bit
//! `.wav`, which every program opens but is several times larger.

pub mod decode;
pub mod model;
pub mod ogg;
pub mod raw;
pub mod vorbis;
pub mod wav;

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use crate::cache::package::Part;
use crate::export::{ExportError, ExportInput, Exporter, Output};
use crate::types::Kind;

pub use decode::{Pcm, decode, no_headers, vorbis_headers};
pub use model::{Audio, AudioError, Codec};
pub use raw::AudioHeader;
pub use vorbis::{VorbisHeaders, VorbisTables};

/// What a sound is exported as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AudioOutput {
    /// The sound's own format: `.ogg`, `.opus` or `.wav`.
    #[default]
    Native,
    /// Decoded to 16-bit PCM.
    Wav,
}

impl AudioOutput {
    pub fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Wav => "wav",
        }
    }
}

impl fmt::Display for AudioOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for AudioOutput {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "native" => Ok(Self::Native),
            "wav" => Ok(Self::Wav),
            _ => Err(format!("unknown audio format {s:?} (native or wav)")),
        }
    }
}

/// Read one sound: its header from H, its data from B and F.
pub fn read(input: &ExportInput) -> Result<Audio, ExportError> {
    let header = raw::parse(input.h_body()).map_err(|e| ExportError::failed(e.to_string()))?;
    let part = |part: Part| if input.has(part) { input.read(part) } else { Ok(Vec::new()) };
    let (b, f) = (part(Part::B)?, part(Part::F)?);
    Audio::assemble(&header, &b, &f).map_err(|e| match e {
        AudioError::UnknownCodec(_) | AudioError::Adpcm | AudioError::PcmBits(_) => ExportError::unsupported(e.to_string()),
        _ => ExportError::failed(e.to_string()),
    })
}

/// `/A/B.wav` as `/A/B.<extension>`; a path that doesn't end in `.wav` keeps its whole
/// name.
pub fn output_path(path: &str, extension: &str) -> String {
    let stem = match path.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("wav") => stem,
        _ => path,
    };
    format!("{stem}.{extension}")
}

pub struct AudioExporter {
    pub output: AudioOutput,
    /// The Vorbis header sets to use instead of the bundled ones.
    pub vorbis: Option<Arc<VorbisTables>>,
}

impl Exporter for AudioExporter {
    /// The output format is part of the name, so switching formats exports again.
    fn name(&self) -> &'static str {
        match self.output {
            AudioOutput::Native => "audio-native",
            AudioOutput::Wav => "audio-wav",
        }
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> Kind {
        Kind::Audio
    }

    fn export(&self, input: &ExportInput) -> Result<Vec<Output>, ExportError> {
        let audio = read(input)?;
        let tables = self.vorbis.as_deref();
        if audio.codec == Codec::Opus && audio.channels > 2 {
            return Err(ExportError::unsupported(format!("Opus with {} channels has no writer yet", audio.channels)));
        }
        let headers = match audio.codec {
            // A combination wfscan has no headers for is a new kind of sound, not a
            // broken file.
            Codec::Vorbis if tables.unwrap_or_else(|| VorbisTables::bundled()).candidates(audio.channels, audio.sample_rate, audio.long_block).is_empty() => {
                return Err(ExportError::unsupported(format!("{} (if a game update added one, --game-exe reads the game's own)", no_headers(&audio))));
            }
            Codec::Vorbis => Some(vorbis_headers(&audio, tables).map_err(ExportError::failed)?),
            _ => None,
        };
        let (extension, data) = match (self.output, audio.codec) {
            (AudioOutput::Native, Codec::Vorbis) => ("ogg", ogg::vorbis(&audio, headers.expect("chosen above"))),
            (AudioOutput::Native, Codec::Opus) => ("opus", ogg::opus(&audio)),
            (_, Codec::Pcm) => ("wav", wav::write(u16::from(audio.channels), audio.sample_rate, &audio.data[..pcm_len(&audio)])),
            (AudioOutput::Wav, _) if !cfg!(feature = "audio") => {
                return Err(ExportError::unsupported("this wfscan was built without audio decoding (rebuild with the `audio` feature, or use the native format)"));
            }
            (AudioOutput::Wav, _) => {
                let pcm = decode(&audio, tables).map_err(ExportError::failed)?;
                ("wav", wav::write_samples(pcm.channels, pcm.sample_rate, &pcm.samples))
            }
        };
        Ok(vec![Output { path: output_path(input.path, extension), data }])
    }
}

/// The bytes of a PCM sound's data that its sample count covers.
fn pcm_len(audio: &Audio) -> usize {
    (audio.samples as usize * usize::from(audio.channels) * 2).min(audio.data.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_paths() {
        assert_eq!(output_path("/Lotus/Sounds/A.wav", "ogg"), "/Lotus/Sounds/A.ogg");
        assert_eq!(output_path("/Lotus/Sounds/A.WAV", "opus"), "/Lotus/Sounds/A.opus");
        assert_eq!(output_path("/Lotus/Sounds/A", "wav"), "/Lotus/Sounds/A.wav");
        assert_eq!(output_path("/Lotus/Sounds/A.v2", "ogg"), "/Lotus/Sounds/A.v2.ogg");
    }

    #[test]
    fn output_names() {
        assert_eq!("wav".parse::<AudioOutput>(), Ok(AudioOutput::Wav));
        assert_eq!(AudioOutput::default().to_string(), "native");
        assert!("mp3".parse::<AudioOutput>().is_err());
    }
}
