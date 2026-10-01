//! The audio header: what follows the common header in a sound's H part.
//!
//! Layout, little-endian, 66 bytes (at least 28 more follow that nothing here needs):
//! - u32: codec. 0 PCM, 5 ADPCM, 7 Opus, 8 Vorbis;
//! - u32: a quality level (0, 1 or 2; `SampleQuality=LOW` sounds have 2);
//! - 3 x u32, f32: unknown;
//! - u32: where the data is. 0 all in B, 1 the first part in B and the rest in F, 2 all
//!   in F; u32: unknown (a size near 64 KiB when the data is split or in F);
//! - u32 sample rate, u8 bits per sample, u8 channels;
//! - u32: samples per channel;
//! - u32: a bitrate (Vorbis: 85000 or 50000; Opus: 120);
//! - u16 block size (the size of every Opus packet; 1024 for Vorbis);
//! - u16 samples per block (Opus 960; Vorbis 2048, its long block);
//! - 2 x u32: unknown, zero;
//! - u32: u32s in the Vorbis packet table (zero for the other codecs);
//! - u32: bytes of audio data.
//!
//! Sources: the codec, sample rate, bits, channels, block fields and data size come from
//! the original's audio reader (credited there to sehnryr's lotus-lib). The sample count,
//! the packet table count and codec 8 were worked out from the September 2026 cache: the
//! sample count equals the last entry of every Vorbis packet table.

use crate::formats::reader::{ReadError, Reader};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioHeader {
    pub codec: u32,
    pub quality: u32,
    pub sample_rate: u32,
    pub bits_per_sample: u8,
    pub channels: u8,
    /// Samples per channel.
    pub samples: u32,
    pub bitrate: u32,
    pub block_size: u16,
    pub samples_per_block: u16,
    /// How many u32s the Vorbis packet table holds.
    pub table_len: u32,
    /// Bytes of audio data, at the end of B followed by F.
    pub size: u32,
}

/// Parse the audio header from an H part's bytes after the common header.
pub fn parse(data: &[u8]) -> Result<AudioHeader, ReadError> {
    let mut r = Reader::new(data);
    let codec = r.u32("codec")?;
    let quality = r.u32("quality")?;
    r.skip(24, "unknown fields")?;
    let sample_rate = r.u32("sample rate")?;
    let bits_per_sample = r.u8("bits per sample")?;
    let channels = r.u8("channels")?;
    let samples = r.u32("sample count")?;
    let bitrate = r.u32("bitrate")?;
    let block_size = r.u16("block size")?;
    let samples_per_block = r.u16("samples per block")?;
    r.skip(8, "unknown fields")?;
    let table_len = r.u32("packet table length")?;
    let size = r.u32("data size")?;
    Ok(AudioHeader { codec, quality, sample_rate, bits_per_sample, channels, samples, bitrate, block_size, samples_per_block, table_len, size })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_header() {
        // /EE/Sounds/Impacts/Glass/GlassImpactA.wav: Vorbis, 32 kHz mono, 30,838 samples.
        #[rustfmt::skip]
        let bytes = [
            8, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x3f, 0, 0, 0, 0, 0, 0, 0, 0,
            0x00, 0x7d, 0, 0, 0x10, 0x01, 0x76, 0x78, 0, 0, 0x50, 0xc3, 0, 0, 0x00, 0x04, 0x00, 0x08,
            0, 0, 0, 0, 0, 0, 0, 0, 0xe6, 0, 0, 0, 0xfe, 0x28, 0, 0, 0xb4, 0x50, 0x8e, 0xc1,
        ];
        let h = parse(&bytes).unwrap();
        assert_eq!((h.codec, h.quality, h.sample_rate, h.bits_per_sample, h.channels), (8, 2, 32000, 16, 1));
        assert_eq!((h.samples, h.bitrate, h.block_size, h.samples_per_block, h.table_len, h.size), (30838, 50000, 1024, 2048, 230, 10494));
        assert!(matches!(parse(&bytes[..65]), Err(ReadError::Truncated { .. })));
        assert!(parse(&bytes[..66]).is_ok());
    }
}
