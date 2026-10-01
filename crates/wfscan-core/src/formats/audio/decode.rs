//! Decoding a sound to 16-bit PCM, for playback. Vorbis is decoded by `lewton` and Opus
//! by `ropus`, both behind the `audio` cargo feature.

use super::model::{Audio, Codec};
use super::vorbis::{VorbisHeaders, VorbisTables};

/// Decoded audio: interleaved 16-bit samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    pub channels: u16,
    pub sample_rate: u32,
    pub samples: Vec<i16>,
}

impl Pcm {
    /// Samples per channel.
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate.max(1))
    }

    /// The loudest sample (as 0-1) in each of `buckets` equal stretches, for a waveform.
    pub fn peaks(&self, buckets: usize) -> Vec<f32> {
        let channels = usize::from(self.channels.max(1));
        let frames = self.frames();
        (0..buckets.min(frames))
            .map(|i| {
                let range = i * frames / buckets.min(frames) * channels..(i + 1) * frames / buckets.min(frames) * channels;
                let peak = self.samples[range].iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                f32::from(peak) / 32768.0
            })
            .collect()
    }
}

/// Decode `audio`. Vorbis sounds use the header sets in `tables`, or the bundled ones.
pub fn decode(audio: &Audio, tables: Option<&VorbisTables>) -> Result<Pcm, String> {
    let channels = u16::from(audio.channels);
    let wanted = audio.samples as usize * usize::from(channels);
    let mut samples = match audio.codec {
        Codec::Pcm => audio.data.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect(),
        Codec::Opus => opus(audio)?,
        Codec::Vorbis => vorbis(audio, tables)?,
    };
    // Opus and Vorbis decode whole blocks, past the end of the sound.
    samples.truncate(wanted);
    Ok(Pcm { channels, sample_rate: audio.sample_rate, samples })
}

#[cfg(feature = "audio")]
fn opus(audio: &Audio) -> Result<Vec<i16>, String> {
    use super::model::OPUS_PACKET_SAMPLES;

    let channels = usize::from(audio.channels);
    if channels > 2 {
        return Err(format!("Opus with {channels} channels isn't decoded"));
    }
    let layout = if channels == 1 { ropus::Channels::Mono } else { ropus::Channels::Stereo };
    let mut decoder = ropus::Decoder::new(audio.sample_rate, layout).map_err(|e| format!("Opus: {e:?}"))?;
    // Room for the longest packet Opus allows (120 ms), though these are 20 ms.
    let mut block = vec![0i16; 6 * OPUS_PACKET_SAMPLES as usize * channels];
    let mut samples = Vec::with_capacity(audio.packets.len() * OPUS_PACKET_SAMPLES as usize * channels);
    for (i, packet) in audio.packets.iter().enumerate() {
        let frames = decoder.decode(&audio.data[packet.clone()], &mut block, ropus::DecodeMode::Normal).map_err(|e| format!("Opus packet {i}: {e:?}"))?;
        samples.extend_from_slice(&block[..frames * channels]);
    }
    // The packets hold more than the sound: drop the decoder's lead-in from the start.
    samples.drain(..(audio.opus_pre_skip() * channels).min(samples.len()));
    Ok(samples)
}

/// The header set a Vorbis sound was encoded with. Where the game has two setups for a
/// sound's channels and sample rate, this decodes the start of the sound with each and
/// takes the one whose result clips least, then the quieter one: with the wrong setup a
/// sound is loud noise. (Without the `audio` feature it takes the later setup, which
/// most sounds use.)
///
/// `tables` are the sets to choose from: the bundled ones if `None`.
pub fn vorbis_headers<'a>(audio: &Audio, tables: Option<&'a VorbisTables>) -> Result<&'a VorbisHeaders, String> {
    let tables = tables.unwrap_or_else(|| VorbisTables::bundled());
    let candidates = tables.candidates(audio.channels, audio.sample_rate, audio.long_block);
    match candidates.len() {
        0 => Err(no_headers(audio)),
        1 => Ok(candidates[0]),
        _ => choose(audio, &candidates),
    }
}

/// What to say about a Vorbis sound no header set fits.
pub fn no_headers(audio: &Audio) -> String {
    format!("no Vorbis header set for {} channels at {} Hz with {}-sample blocks", audio.channels, audio.sample_rate, audio.long_block)
}

#[cfg(feature = "audio")]
fn choose<'a>(audio: &Audio, candidates: &[&'a VorbisHeaders]) -> Result<&'a VorbisHeaders, String> {
    let mut best: Option<((u64, f64), &VorbisHeaders)> = None;
    let mut error = String::new();
    for &set in candidates {
        match vorbis_with(audio, set, TRIAL_PACKETS) {
            Ok(samples) => {
                let clipped = samples.iter().filter(|&&s| s == i16::MAX || s == i16::MIN).count() as u64;
                let energy: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
                if best.as_ref().is_none_or(|(score, _)| (clipped, energy) < *score) {
                    best = Some(((clipped, energy), set));
                }
            }
            Err(e) => error = e,
        }
    }
    best.map(|(_, set)| set).ok_or(error)
}

#[cfg(not(feature = "audio"))]
fn choose<'a>(_audio: &Audio, candidates: &[&'a VorbisHeaders]) -> Result<&'a VorbisHeaders, String> {
    Ok(candidates[candidates.len() - 1])
}

#[cfg(feature = "audio")]
fn vorbis(audio: &Audio, tables: Option<&VorbisTables>) -> Result<Vec<i16>, String> {
    vorbis_with(audio, vorbis_headers(audio, tables)?, audio.packets.len())
}

/// How many packets of a Vorbis sound are decoded with each candidate setup to choose
/// between them (about nine seconds).
#[cfg(feature = "audio")]
const TRIAL_PACKETS: usize = 400;

/// Decode the first `packets` packets with one header set.
#[cfg(feature = "audio")]
fn vorbis_with(audio: &Audio, headers: &VorbisHeaders, packets: usize) -> Result<Vec<i16>, String> {
    use lewton::audio::{PreviousWindowRight, read_audio_packet};
    use lewton::header::{read_header_ident, read_header_setup};

    let ident = read_header_ident(&headers.ident).map_err(|e| format!("Vorbis identification header: {e}"))?;
    let setup = read_header_setup(&headers.setup, ident.audio_channels, (ident.blocksize_0, ident.blocksize_1)).map_err(|e| format!("Vorbis setup header: {e}"))?;
    let mut previous = PreviousWindowRight::new();
    let mut samples = Vec::new();
    for (i, packet) in audio.packets.iter().take(packets).enumerate() {
        let decoded = read_audio_packet(&ident, &setup, &audio.data[packet.clone()], &mut previous).map_err(|e| format!("Vorbis packet {i}: {e}"))?;
        let frames = decoded.first().map_or(0, Vec::len);
        for frame in 0..frames {
            samples.extend(decoded.iter().map(|channel| channel[frame]));
        }
    }
    Ok(samples)
}

#[cfg(not(feature = "audio"))]
fn opus(_audio: &Audio) -> Result<Vec<i16>, String> {
    Err("this wfscan was built without audio decoding (rebuild with the `audio` feature)".into())
}

#[cfg(not(feature = "audio"))]
fn vorbis(_audio: &Audio, _tables: Option<&VorbisTables>) -> Result<Vec<i16>, String> {
    Err("this wfscan was built without audio decoding (rebuild with the `audio` feature)".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_is_read_as_it_is() {
        let audio = Audio {
            codec: Codec::Pcm,
            channels: 2,
            sample_rate: 8000,
            samples: 2,
            quality: 0,
            bitrate: 0,
            long_block: 0,
            data: vec![0x34, 0x12, 0xff, 0xff, 1, 0, 2, 0, 9, 9, 9, 9],
            packets: Vec::new(),
            positions: Vec::new(),
        };
        let pcm = decode(&audio, None).unwrap();
        assert_eq!((pcm.channels, pcm.sample_rate, &pcm.samples[..]), (2, 8000, &[0x1234, -1, 1, 2][..]), "cut at the sample count");
        assert_eq!((pcm.frames(), pcm.seconds()), (2, 0.00025));
    }

    #[test]
    fn peaks_cover_the_sound() {
        let pcm = Pcm { channels: 1, sample_rate: 8000, samples: vec![0, 16384, -32768, 0, 100, -200, 0, 0] };
        assert_eq!(pcm.peaks(4), [0.5, 1.0, 200.0 / 32768.0, 0.0]);
        assert_eq!(pcm.peaks(100).len(), 8, "no more buckets than samples");
        assert!(Pcm { channels: 2, sample_rate: 8000, samples: Vec::new() }.peaks(10).is_empty());
    }

    #[test]
    fn vorbis_without_tables_says_what_is_missing() {
        let audio = Audio { codec: Codec::Vorbis, channels: 1, sample_rate: 44100, samples: 10, quality: 1, bitrate: 85000, long_block: 2048, data: vec![0; 4], packets: vec![0..2, 2..4], positions: vec![0, 10] };
        assert_eq!(vorbis_headers(&audio, Some(&VorbisTables::default())).unwrap_err(), "no Vorbis header set for 1 channels at 44100 Hz with 2048-sample blocks");
        // The bundled sets have two for it; noise decodes with either, so one is chosen.
        assert!(vorbis_headers(&audio, None).is_ok());
    }
}
