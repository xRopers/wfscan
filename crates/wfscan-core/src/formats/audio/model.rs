//! A sound, free of how the cache stores it: its codec, shape, and the audio data cut
//! into packets.
//!
//! The audio data is the last `size` bytes of B followed by F (either may be missing).
//! What comes before it in B is other data: for Vorbis the packet table, for some
//! dialogue what is probably lip-sync data.
//!
//! - PCM: 16-bit little-endian samples, interleaved.
//! - Opus: packets of one fixed size (the header's block size), 20 ms each at 48 kHz, with
//!   no container. The packets hold up to a packet more than the header's sample count.
//!   The original drops 312 samples from the start (an Opus encoder's usual lead-in);
//!   some sounds have fewer than that to spare (51 of 229 checked had 256), so the
//!   decoder drops 312 or what there is to spare, whichever is less. Counts in the
//!   header are at the sound's own sample rate, which for some dialogue is 24 kHz.
//! - Vorbis: packets of varying size with no container and no header packets (see
//!   [`super::vorbis`]). Just before the data is a table of u32s: a u16 packet count and
//!   a u16 chunk count; one u32 per chunk (a coarse seek index, one entry per 64 KiB of
//!   data); each packet's offset in the data; and for each packet a sample position,
//!   the last of which is the sound's sample count.
//!
//! Sources: the data rule and the Opus layout follow the original's readers; the Vorbis
//! table was worked out from the September 2026 cache, where the header's table length
//! is `1 + chunks + 2 x packets` for every Vorbis sound and all of them decode.

use std::ops::Range;

use super::raw::AudioHeader;

/// Samples (at 48 kHz) at the start of a decoded Opus stream that aren't part of the
/// sound, at most.
pub const OPUS_PRE_SKIP: usize = 312;
/// Samples (per channel, at 48 kHz) in a 20 ms Opus packet, which nearly all are.
pub const OPUS_PACKET_SAMPLES: u32 = 960;

/// Samples (per channel, at 48 kHz) an Opus packet decodes to, from its first byte
/// (RFC 6716, section 3.1): the frame length its configuration number stands for,
/// times how many frames it holds. Most of the game's packets are one 20 ms frame, but
/// not all: 18 sounds in the September 2026 cache have shorter ones.
pub fn opus_packet_samples(packet: &[u8]) -> u32 {
    let Some(&toc) = packet.first() else { return 0 };
    let config = usize::from(toc >> 3);
    let frame = match config {
        0..=11 => [480, 960, 1920, 2880][config % 4],
        12..=15 => [480, 960][config % 2],
        _ => [120, 240, 480, 960][config % 4],
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => u32::from(packet.get(1).copied().unwrap_or(0) & 0x3f),
    };
    frame * frames
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Pcm,
    Opus,
    Vorbis,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::Pcm => "PCM",
            Codec::Opus => "Opus",
            Codec::Vorbis => "Vorbis",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    #[error("audio codec {0} is not known")]
    UnknownCodec(u32),
    #[error("ADPCM sounds have no reader yet")]
    Adpcm,
    #[error("{0}-bit PCM has no reader yet")]
    PcmBits(u8),
    #[error("the sound has {0} channels")]
    Channels(u8),
    #[error("the sound's sample rate is {0}")]
    SampleRate(u32),
    #[error("the header says {size} bytes of audio, B and F hold {have}")]
    Short { size: u32, have: usize },
    #[error("the Opus data ({size} bytes) isn't whole packets of {block}")]
    OpusPackets { size: u32, block: u16 },
    #[error("the Vorbis packet table is wrong: {0}")]
    Table(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audio {
    pub codec: Codec,
    pub channels: u8,
    pub sample_rate: u32,
    /// Samples per channel.
    pub samples: u32,
    /// The header's quality level and bitrate, for display.
    pub quality: u32,
    pub bitrate: u32,
    /// Vorbis: samples in a long block.
    pub long_block: u32,
    pub data: Vec<u8>,
    /// Where each packet is in `data`, in order. Empty for PCM.
    pub packets: Vec<Range<usize>>,
    /// Vorbis: the sample position the packet table gives for each packet.
    pub positions: Vec<u64>,
}

impl Audio {
    /// Put a sound together from its header and its B and F parts (empty if missing).
    pub fn assemble(header: &AudioHeader, b: &[u8], f: &[u8]) -> Result<Self, AudioError> {
        let codec = match header.codec {
            0 => Codec::Pcm,
            5 => return Err(AudioError::Adpcm),
            7 => Codec::Opus,
            8 => Codec::Vorbis,
            other => return Err(AudioError::UnknownCodec(other)),
        };
        if !(1..=8).contains(&header.channels) {
            return Err(AudioError::Channels(header.channels));
        }
        if !(1000..=384_000).contains(&header.sample_rate) {
            return Err(AudioError::SampleRate(header.sample_rate));
        }
        let size = header.size as usize;
        let have = b.len() + f.len();
        let start = have.checked_sub(size).ok_or(AudioError::Short { size: header.size, have })?;
        // The bytes from `start` of B followed by F, and the table before them.
        let mut data = Vec::with_capacity(size);
        data.extend_from_slice(b.get(start..).unwrap_or(&[]));
        data.extend_from_slice(&f[start.saturating_sub(b.len())..]);

        let mut positions = Vec::new();
        let packets = match codec {
            Codec::Pcm if header.bits_per_sample != 16 => return Err(AudioError::PcmBits(header.bits_per_sample)),
            Codec::Pcm => Vec::new(),
            Codec::Opus => {
                let block = usize::from(header.block_size);
                if block == 0 || !size.is_multiple_of(block) {
                    return Err(AudioError::OpusPackets { size: header.size, block: header.block_size });
                }
                (0..size / block).map(|i| i * block..(i + 1) * block).collect()
            }
            Codec::Vorbis => {
                let table_bytes = header.table_len as usize * 4;
                let table = start.checked_sub(table_bytes).and_then(|at| b.get(at..start)).ok_or(AudioError::Table("it isn't in B before the data"))?;
                let (packets, ends) = vorbis_packets(table, size)?;
                positions = ends;
                packets
            }
        };
        Ok(Self {
            codec,
            channels: header.channels,
            sample_rate: header.sample_rate,
            samples: header.samples,
            quality: header.quality,
            bitrate: header.bitrate,
            long_block: u32::from(header.samples_per_block),
            data,
            packets,
            positions,
        })
    }

    /// Opus: how many samples the packets decode to in all, at the sound's sample rate
    /// (Opus itself counts at 48 kHz; some dialogue is 24 kHz).
    pub fn opus_decoded_samples(&self) -> u64 {
        let at_48k: u64 = self.packets.iter().map(|p| u64::from(opus_packet_samples(&self.data[p.clone()]))).sum();
        at_48k * u64::from(self.sample_rate) / 48000
    }

    /// Opus: how many samples at the start of the decoded stream aren't the sound's, at
    /// the sound's sample rate. 312 at 48 kHz, unless the packets hold less than that
    /// beyond the sound's length.
    pub fn opus_pre_skip(&self) -> usize {
        let usual = OPUS_PRE_SKIP as u64 * u64::from(self.sample_rate) / 48000;
        self.opus_decoded_samples().saturating_sub(u64::from(self.samples)).min(usual) as usize
    }

    pub fn seconds(&self) -> f64 {
        f64::from(self.samples) / f64::from(self.sample_rate)
    }
}

/// The packet ranges a Vorbis table lists for `size` bytes of data, and each packet's
/// sample position.
fn vorbis_packets(table: &[u8], size: usize) -> Result<(Vec<Range<usize>>, Vec<u64>), AudioError> {
    let words: Vec<u32> = table.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect();
    let head = *words.first().ok_or(AudioError::Table("it is empty"))?;
    let (count, chunks) = ((head & 0xffff) as usize, (head >> 16) as usize);
    if words.len() != 1 + chunks + 2 * count {
        return Err(AudioError::Table("its length doesn't fit its packet count"));
    }
    let offsets = &words[1 + chunks..1 + chunks + count];
    let mut packets = Vec::with_capacity(count);
    for (i, &offset) in offsets.iter().enumerate() {
        let end = offsets.get(i + 1).map_or(size, |&next| next as usize);
        if offset as usize >= end || end > size {
            return Err(AudioError::Table("a packet's offset is out of order or past the data"));
        }
        packets.push(offset as usize..end);
    }
    let positions = words[1 + chunks + count..].iter().map(|&p| u64::from(p)).collect();
    Ok((packets, positions))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(codec: u32, size: u32) -> AudioHeader {
        AudioHeader { codec, quality: 1, sample_rate: 48000, bits_per_sample: 16, channels: 2, samples: 100, bitrate: 0, block_size: 4, samples_per_block: 960, table_len: 0, size }
    }

    #[test]
    fn data_is_the_end_of_b_then_f() {
        let pcm = |b: &[u8], f: &[u8], size| Audio::assemble(&header(0, size), b, f).map(|a| a.data);
        assert_eq!(pcm(&[9, 9, 1, 2], &[], 2).unwrap(), [1, 2]);
        assert_eq!(pcm(&[9, 1, 2], &[3, 4], 4).unwrap(), [1, 2, 3, 4]);
        assert_eq!(pcm(&[9, 9], &[3, 4], 2).unwrap(), [3, 4], "B holds only other data");
        assert_eq!(pcm(&[], &[3, 4], 2).unwrap(), [3, 4]);
        assert_eq!(pcm(&[1], &[2], 3), Err(AudioError::Short { size: 3, have: 2 }));
    }

    #[test]
    fn opus_packets_are_blocks() {
        // Packets that start 0xf8: one 20 ms frame each.
        let mut audio = Audio::assemble(&header(7, 8), &[0; 3], &[0xf8; 8]).unwrap();
        assert_eq!((audio.codec, &audio.packets[..]), (Codec::Opus, &[0..4, 4..8][..]));
        // Two packets are 1920 samples: plenty to spare over 100, less over 1700.
        assert_eq!((audio.opus_decoded_samples(), audio.opus_pre_skip()), (1920, 312));
        audio.samples = 1700;
        assert_eq!(audio.opus_pre_skip(), 220);
        // A 10 ms packet leaves less.
        audio.data[4] = 0xf0;
        assert_eq!((audio.opus_decoded_samples(), audio.opus_pre_skip()), (1440, 0));
        // At 24 kHz everything is half: 720 samples decoded, and 156 the usual lead-in.
        audio.sample_rate = 24000;
        audio.samples = 500;
        assert_eq!((audio.opus_decoded_samples(), audio.opus_pre_skip()), (720, 156));
        assert_eq!(Audio::assemble(&header(7, 6), &[], &[1; 6]), Err(AudioError::OpusPackets { size: 6, block: 4 }));
    }

    #[test]
    fn vorbis_packets_come_from_the_table() {
        // Three packets, one chunk entry.
        let words: [u32; 8] = [3 | 1 << 16, 77, 0, 2, 5, 0, 128, 700];
        let mut b: Vec<u8> = vec![0xee; 5];
        b.extend(words.iter().flat_map(|w| w.to_le_bytes()));
        b.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        let mut h = header(8, 6);
        h.table_len = 8;
        let audio = Audio::assemble(&h, &b, &[]).unwrap();
        assert_eq!((audio.data.len(), &audio.packets[..], &audio.positions[..]), (6, &[0..2, 2..5, 5..6][..], &[0, 128, 700][..]));
        // The same with the data in F.
        let audio = Audio::assemble(&h, &b[..b.len() - 6], &[1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(audio.packets.len(), 3);
        h.table_len = 7;
        assert_eq!(Audio::assemble(&h, &b, &[]), Err(AudioError::Table("its length doesn't fit its packet count")));
        h.table_len = 100;
        assert_eq!(Audio::assemble(&h, &b, &[]), Err(AudioError::Table("it isn't in B before the data")));
    }

    #[test]
    fn opus_packet_lengths_come_from_the_first_byte() {
        assert_eq!(opus_packet_samples(&[0xf8]), 960); // CELT fullband 20 ms, one frame
        assert_eq!(opus_packet_samples(&[0xf0]), 480); // 10 ms
        assert_eq!(opus_packet_samples(&[0xf9]), 1920); // two frames
        assert_eq!(opus_packet_samples(&[0x1b, 0x03]), 2880 * 3); // SILK 60 ms, three
        assert_eq!(opus_packet_samples(&[0x60]), 480); // hybrid 10 ms
        assert_eq!(opus_packet_samples(&[]), 0);
    }

    #[test]
    fn what_has_no_reader_says_so() {
        assert_eq!(Audio::assemble(&header(5, 0), &[], &[]), Err(AudioError::Adpcm));
        assert_eq!(Audio::assemble(&header(9, 0), &[], &[]), Err(AudioError::UnknownCodec(9)));
    }
}
