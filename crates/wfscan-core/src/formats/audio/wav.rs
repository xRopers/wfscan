//! WAV files: 16-bit PCM behind the 44-byte RIFF header every player reads.

/// A WAV file of interleaved 16-bit little-endian samples given as bytes.
pub fn write(channels: u16, sample_rate: u32, pcm: &[u8]) -> Vec<u8> {
    let block = u32::from(channels) * 2;
    // Whole frames only.
    let pcm = &pcm[..pcm.len() - pcm.len() % (block.max(1) as usize)];
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * block).to_le_bytes());
    out.extend_from_slice(&(block as u16).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// A WAV file of interleaved 16-bit samples.
pub fn write_samples(channels: u16, sample_rate: u32, samples: &[i16]) -> Vec<u8> {
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    write(channels, sample_rate, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_describes_the_samples() {
        let wav = write_samples(2, 8000, &[0x1234, -1, 1, 2]);
        assert_eq!(wav.len(), 44 + 8);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        // PCM, 2 channels, 8000 Hz, 32000 bytes a second, 4 bytes a frame, 16 bits.
        assert_eq!(wav[20..36], [1, 0, 2, 0, 0x40, 0x1f, 0, 0, 0x00, 0x7d, 0, 0, 4, 0, 16, 0]);
        assert_eq!((&wav[36..40], u32::from_le_bytes(wav[40..44].try_into().unwrap())), (&b"data"[..], 8));
        assert_eq!(wav[44..], [0x34, 0x12, 0xff, 0xff, 1, 0, 2, 0]);
        // Half a frame at the end is dropped.
        assert_eq!(write(2, 8000, &[1, 2, 3, 4, 5, 6]).len(), 44 + 4);
    }
}
