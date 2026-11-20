//! Videos: Bink files in the `VideoTexture` packages.
//!
//! As `.bk2` (the default) a video is copied out as it is, after checking its header.
//! As PNG frames it is decoded by the user's own `bink2w64.dll` (see [`decode`]) into a
//! folder named after the video: `00000.png`, `00001.png`, ... and a `video.json` with the
//! frame rate, size and frame count, so the frames can be put back together at the right
//! speed (FFmpeg: `ffmpeg -framerate 30 -i %05d.png out.mp4`). Each frame is written as
//! soon as it is decoded, so a long video never sits in memory. Sound tracks are only in
//! the `.bk2`.

pub mod bink;
#[cfg(feature = "bink")]
pub mod decode;
#[cfg(not(feature = "bink"))]
#[path = "no_decode.rs"]
pub mod decode;

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde_json::json;

pub use bink::BinkHeader;
pub use decode::{BINK_DLL_ENV, BinkLibrary};

use crate::export::{ExportError, ExportInput, Exporter, Output};
use crate::formats::passthrough::output_path;
use crate::types::Kind;

/// What a video is exported as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VideoOutput {
    /// The Bink file as it is.
    #[default]
    Bink,
    /// Every frame as a PNG, through the Bink library.
    Png,
}

impl VideoOutput {
    pub fn name(self) -> &'static str {
        match self {
            Self::Bink => "bk2",
            Self::Png => "png",
        }
    }
}

impl fmt::Display for VideoOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for VideoOutput {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "bk2" => Ok(Self::Bink),
            "png" => Ok(Self::Png),
            _ => Err(format!("unknown video format {s:?} (bk2 or png)")),
        }
    }
}

/// `/A/Intro.bk2` as `/A/Intro`: the folder a video's frames go in.
pub fn frames_folder(path: &str) -> String {
    let name_start = path.rfind('/').map_or(0, |i| i + 1);
    match path[name_start..].rfind('.') {
        Some(dot) if ["bk2", "bik"].iter().any(|e| e.eq_ignore_ascii_case(&path[name_start + dot + 1..])) => path[..name_start + dot].to_string(),
        _ => path.to_string(),
    }
}

pub struct VideoExporter {
    pub output: VideoOutput,
    /// Needed for PNG frames.
    pub bink: Option<Arc<BinkLibrary>>,
}

impl Exporter for VideoExporter {
    /// The output format is part of the name, so switching formats exports again.
    fn name(&self) -> &'static str {
        match self.output {
            VideoOutput::Bink => "video-bk2",
            VideoOutput::Png => "video-png",
        }
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> Kind {
        Kind::Video
    }

    fn export(&self, input: &ExportInput) -> Result<Vec<Output>, ExportError> {
        let mut outputs = Vec::new();
        self.export_each(input, &mut |output| {
            outputs.push(output);
            Ok(())
        })?;
        Ok(outputs)
    }

    fn export_each(&self, input: &ExportInput, emit: &mut dyn FnMut(Output) -> Result<(), ExportError>) -> Result<(), ExportError> {
        let (data, header) = bink::read(input)?;
        match self.output {
            VideoOutput::Bink => {
                let extension = if header.is_bink2() { "bk2" } else { "bik" };
                emit(Output { path: output_path(input.path, extension, &["bk2", "bik"]), data })
            }
            VideoOutput::Png => self.frames(input.path, &data, &header, emit),
        }
    }
}

impl VideoExporter {
    fn frames(&self, path: &str, data: &[u8], header: &BinkHeader, emit: &mut dyn FnMut(Output) -> Result<(), ExportError>) -> Result<(), ExportError> {
        let Some(bink) = &self.bink else {
            return Err(ExportError::unsupported(format!(
                "PNG frames need the Bink library (--bink-dll PATH or {BINK_DLL_ENV}; the game has one at Warframe\\Tools\\Bink2\\bink2w64.dll)"
            )));
        };
        if !cfg!(feature = "png") {
            return Err(ExportError::unsupported("this wfscan was built without PNG support (rebuild with the `png` feature)"));
        }
        let folder = frames_folder(path);
        let info = json!({
            "source": path,
            "width": header.width,
            "height": header.height,
            "frames": header.frames,
            "frame_rate": header.frame_rate(),
            "frame_rate_fraction": [header.rate_numerator, header.rate_denominator],
            "audio_tracks": header.audio_tracks,
            "frame_files": "%05d.png",
        });
        let info = serde_json::to_vec_pretty(&info).map_err(|e| ExportError::failed(e.to_string()))?;
        emit(Output { path: format!("{folder}/video.json"), data: info })?;
        let mut stopped: Option<ExportError> = None;
        let decoded = bink.frames(data, header, |index, rgba| {
            let png = encode_png(header.width, header.height, rgba);
            match emit(Output { path: format!("{folder}/{index:05}.png"), data: png }) {
                Ok(()) => Ok(true),
                Err(e) => {
                    stopped = Some(e);
                    Err("stopped".into())
                }
            }
        });
        match (stopped, decoded) {
            (Some(e), _) => Err(e),
            (None, Err(e)) => Err(ExportError::failed(e)),
            (None, Ok(())) => Ok(()),
        }
    }
}

/// A frame as an RGB PNG (video frames are opaque) with fast compression: a frame is
/// encoded in a few milliseconds instead of tens.
#[cfg(feature = "png")]
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().expect("writing to a Vec cannot fail");
    writer.write_image_data(&rgb).expect("the frame size matches its dimensions");
    writer.finish().expect("writing to a Vec cannot fail");
    out
}

#[cfg(not(feature = "png"))]
fn encode_png(_width: u32, _height: u32, _rgba: &[u8]) -> Vec<u8> {
    unreachable!("checked before decoding")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(frames_folder("/Lotus/Fx/Intro.bk2"), "/Lotus/Fx/Intro");
        assert_eq!(frames_folder("/Lotus/Fx/Old.BIK"), "/Lotus/Fx/Old");
        assert_eq!(frames_folder("/Lotus/Fx/NoExtension"), "/Lotus/Fx/NoExtension");
        assert_eq!("png".parse::<VideoOutput>(), Ok(VideoOutput::Png));
        assert_eq!(VideoOutput::default().to_string(), "bk2");
        assert!("mp4".parse::<VideoOutput>().unwrap_err().contains("bk2 or png"));
    }
}
