//! The Bink file header, read by wfscan itself, and where a video's Bink file is.
//!
//! Layout (little-endian), the public Bink container header: a 3-byte magic (`KB2` for
//! Bink 2, `BIK` for Bink 1) and a revision letter; u32 file size minus 8; u32 frame
//! count; u32 largest frame size; u32 frame count again; u32 width; u32 height; u32 frame
//! rate numerator and denominator; u32 flags; u32 audio track count.
//!
//! Checked on the November 2026 cache: all 238 `.bk2` files of the `VideoTexture*`
//! packages are whole Bink 2 files in F (size field = F's length minus 8), and the
//! dimensions and frame counts read here are what `bink2w64.dll` reports once it has
//! opened them.

use crate::cache::package::Part;
use crate::export::{ExportError, ExportInput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinkHeader {
    /// `KB2` or `BIK`.
    pub magic: [u8; 3],
    pub frames: u32,
    pub width: u32,
    pub height: u32,
    pub rate_numerator: u32,
    pub rate_denominator: u32,
    pub audio_tracks: u32,
}

impl BinkHeader {
    pub fn is_bink2(&self) -> bool {
        self.magic == *b"KB2"
    }

    pub fn frame_rate(&self) -> f64 {
        f64::from(self.rate_numerator) / f64::from(self.rate_denominator.max(1))
    }

    pub fn seconds(&self) -> f64 {
        f64::from(self.frames) / self.frame_rate().max(f64::MIN_POSITIVE)
    }
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(offset..offset + 4)?.try_into().ok()?))
}

/// The header of a whole Bink file, or why `data` isn't one.
pub fn parse(data: &[u8]) -> Result<BinkHeader, String> {
    let magic: [u8; 3] = match data.get(..3) {
        Some(m) if m == b"KB2" || m == b"BIK" => m.try_into().expect("three bytes"),
        _ => return Err("no Bink data (KB2 or BIK)".into()),
    };
    let field = |offset| u32_at(data, offset).ok_or_else(|| "the Bink header is cut short".to_string());
    let stated = field(4)? as usize + 8;
    if stated != data.len() {
        return Err(format!("the Bink header says {stated} bytes, the part holds {}", data.len()));
    }
    Ok(BinkHeader {
        magic,
        frames: field(8)?,
        width: field(20)?,
        height: field(24)?,
        rate_numerator: field(28)?,
        rate_denominator: field(32)?,
        audio_tracks: field(40)?,
    })
}

/// The part holding a video's whole Bink file (F, else B), and its header.
pub fn read(input: &ExportInput) -> Result<(Vec<u8>, BinkHeader), ExportError> {
    for part in [Part::F, Part::B] {
        if !input.has(part) {
            continue;
        }
        let data = input.read(part)?;
        if !(data.starts_with(b"KB2") || data.starts_with(b"BIK")) {
            continue;
        }
        return match parse(&data) {
            Ok(header) => Ok((data, header)),
            Err(e) => Err(ExportError::failed(format!("{part}: {e}"))),
        };
    }
    Err(ExportError::unsupported("no Bink data (KB2 or BIK) in B or F"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header of `/Lotus/Interface/AbilityVideos/AshPower1_s.bk2`, then filler.
    fn ash() -> Vec<u8> {
        let mut data = vec![0x4b, 0x42, 0x32, 0x6a];
        let len = 64u32;
        data.extend_from_slice(&(len - 8).to_le_bytes());
        for value in [338u32, 16088, 338, 640, 352, 30, 1, 0x12, 0] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        data.resize(len as usize, 0);
        data
    }

    #[test]
    fn a_real_header() {
        let h = parse(&ash()).unwrap();
        assert!(h.is_bink2());
        assert_eq!((h.frames, h.width, h.height, h.frame_rate(), h.audio_tracks), (338, 640, 352, 30.0, 0));
        assert!((h.seconds() - 11.2667).abs() < 0.001);
        let mut short = ash();
        short.pop();
        assert!(parse(&short).unwrap_err().contains("says 64 bytes, the part holds 63"));
        assert!(parse(b"RIFF....").unwrap_err().contains("no Bink data"));
    }
}
