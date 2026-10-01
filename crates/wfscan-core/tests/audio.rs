//! Sounds, read and decoded: on the synthetic `sounds` fixture, where every sample is
//! known, and on a real cache with the game's own Vorbis tables.

use std::sync::Arc;

use wfscan_core::formats::audio::{self, Codec, VorbisHeaders, VorbisTables, ogg};
use wfscan_core::{AudioOutput, Cache, ExportError, ExportOptions, Exporters, FormatOptions, IndexOptions, Kind, Manifest, Status, export, index, with_input};
use wfscan_fixtures::{self as fx, snd};

fn cache() -> (tempfile::TempDir, Cache) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Cache.Windows");
    fx::sounds().write(&dir).unwrap();
    let cache = Cache::open(&dir).unwrap();
    (tmp, cache)
}

#[test]
fn pcm_sounds_decode_to_their_samples() {
    let (_tmp, cache) = cache();
    for (path, channels, frames) in [(snd::TONE, 1usize, 4000usize), (snd::SPLIT, 2, 2000), (snd::STREAMED, 2, 1000)] {
        let sound = with_input(&cache, "Misc", path, None, audio::read).unwrap().unwrap();
        assert_eq!((sound.codec, usize::from(sound.channels), sound.sample_rate, sound.samples as usize), (Codec::Pcm, channels, fx::SOUND_RATE, frames), "{path}");
        let pcm = audio::decode(&sound, None).unwrap();
        let expected: Vec<i16> = (0..frames).flat_map(|f| (0..channels).map(move |c| fx::sound_sample(f, c))).collect();
        assert_eq!(pcm.samples, expected, "{path}");
        assert_eq!((pcm.frames(), pcm.seconds()), (frames, frames as f64 / 8000.0));
    }
}

#[test]
fn what_cannot_be_read_says_why() {
    let (_tmp, cache) = cache();
    let read = |path: &str| with_input(&cache, "Misc", path, None, audio::read).unwrap();
    assert!(matches!(read(snd::ADPCM), Err(ExportError::Unsupported(r)) if r == "ADPCM sounds have no reader yet"));
    assert!(matches!(read(snd::SHORT), Err(ExportError::Failed(r)) if r == "the header says 800 bytes of audio, B and F hold 700"));

    // The Vorbis sound reads (its table lists three packets), but no header set is for 8 kHz.
    let sound = read(snd::VORBIS).unwrap();
    assert_eq!((sound.codec, &sound.packets[..], sound.samples), (Codec::Vorbis, &[0..20, 20..50, 50..90][..], 700));
    #[cfg(feature = "audio")]
    assert!(audio::decode(&sound, None).unwrap_err().starts_with("no Vorbis header set for 1 channels at 8000 Hz"));
}

#[test]
fn the_fixture_is_indexed_as_audio() {
    let (_tmp, cache) = cache();
    let (manifest, stats) = index(&cache, None, &IndexOptions::default()).unwrap();
    assert_eq!((manifest.files.len(), stats.failed), (6, 0));
    let classifier = manifest.classifier();
    assert!(manifest.files.iter().all(|f| classifier.kind(f) == Some(Kind::Audio)));
}

/// Export the fixture into `<tmp>/out` with these options.
fn exported(formats: &FormatOptions) -> (tempfile::TempDir, Manifest) {
    let (tmp, cache) = cache();
    let (mut manifest, _) = index(&cache, None, &IndexOptions::default()).unwrap();
    let exporters = Exporters::builtin(formats);
    export(&cache, &mut manifest, None, &exporters, None, &tmp.path().join("out"), &ExportOptions::default()).unwrap();
    (tmp, manifest)
}

fn record<'a>(manifest: &'a Manifest, path: &str) -> (Status, &'a str, &'a [String]) {
    let record = manifest.files.iter().find(|f| f.path == path).unwrap().export.as_ref().unwrap();
    (record.status, record.reason.as_deref().unwrap_or(""), &record.outputs)
}

fn output(tmp: &tempfile::TempDir, internal: &str) -> Vec<u8> {
    let path = internal.split('/').filter(|p| !p.is_empty()).fold(tmp.path().join("out"), |dir, part| dir.join(part));
    std::fs::read(path).unwrap()
}

#[test]
fn pcm_sounds_export_as_wav() {
    for format in [AudioOutput::Native, AudioOutput::Wav] {
        let (tmp, manifest) = exported(&FormatOptions { audio: format, ..Default::default() });
        assert_eq!(record(&manifest, snd::SPLIT), (Status::Ok, "", &["/Lotus/Sounds/SplitStereo.wav".to_string()][..]));
        let wav = output(&tmp, "/Lotus/Sounds/SplitStereo.wav");
        let expected: Vec<u8> = (0..2000).flat_map(|f| (0..2).flat_map(move |c| fx::sound_sample(f, c).to_le_bytes())).collect();
        assert_eq!((&wav[..4], &wav[8..16], wav[22], &wav[24..28]), (&b"RIFF"[..], &b"WAVEfmt "[..], 2, &8000u32.to_le_bytes()[..]));
        assert_eq!(wav[44..], expected);

        // What can't be exported says why.
        assert_eq!(record(&manifest, snd::ADPCM), (Status::Unsupported, "ADPCM sounds have no reader yet", &[][..]));
        assert_eq!(record(&manifest, snd::VORBIS), (Status::Unsupported, "no Vorbis header set for 1 channels at 8000 Hz with 2048-sample blocks (if a game update added one, --game-exe reads the game's own)", &[][..]));
        assert_eq!(record(&manifest, snd::SHORT).0, Status::Failed);
        let exporter = manifest.files.iter().find(|f| f.path == snd::TONE).unwrap().export.as_ref().unwrap().exporter.clone();
        assert_eq!(exporter.as_deref(), Some(if format == AudioOutput::Wav { "audio-wav@1" } else { "audio-native@1" }));
    }
}

#[test]
fn a_vorbis_sound_exports_as_ogg_behind_its_header_packets() {
    // Stand-in header packets: the exporter copies them, it doesn't read them.
    let headers = VorbisHeaders {
        channels: 1,
        sample_rate: fx::SOUND_RATE,
        bitrate: 85000,
        block_sizes: (256, 2048),
        ident: b"\x01vorbis-ident".to_vec(),
        comment: b"\x03vorbis-comment".to_vec(),
        setup: [&b"\x05vorbis-setup"[..], &[0x55; 5000]].concat(),
    };
    let tables = Arc::new(VorbisTables { sets: vec![headers.clone()] });
    let (tmp, manifest) = exported(&FormatOptions { vorbis: Some(tables), ..Default::default() });
    assert_eq!(record(&manifest, snd::VORBIS), (Status::Ok, "", &["/Lotus/Sounds/Compressed.ogg".to_string()][..]));

    let file = output(&tmp, "/Lotus/Sounds/Compressed.ogg");
    let (pages, packets) = ogg::parse(&file).expect("well-formed Ogg pages");
    let sound = fx::sounds().packages[0].files.iter().find(|f| f.path == snd::VORBIS).unwrap().parts[1].clone().unwrap().data;
    let data = &sound[sound.len() - 90..];
    assert_eq!(packets, [headers.ident, headers.comment, headers.setup, data[..20].to_vec(), data[20..50].to_vec(), data[50..].to_vec()]);
    // The identification packet has the first page to itself; the last page ends at the
    // sound's length.
    assert_eq!((pages[0].0, pages[0].1, pages.len()), (2, 0, 3));
    assert_eq!((pages[2].0, pages[2].1), (4, 700));
}

/// Real sounds: every sound in a few folders must decode to exactly the number of
/// samples its header states, and sound like something (not noise at full scale).
/// Needs `WFSCAN_CACHE_DIR`, `WFSCAN_OODLE_DLL` and the `audio` feature; skipped
/// otherwise. `WFSCAN_FULL=1` decodes every sound in the cache.
#[cfg(feature = "audio")]
#[test]
fn real_sounds_decode() {
    use std::collections::BTreeMap;
    use std::path::Path;

    use rayon::prelude::*;
    use wfscan_core::OodleDecoder;
    use wfscan_core::formats::audio::VorbisTables;

    let Some(dir) = std::env::var_os("WFSCAN_CACHE_DIR").filter(|d| !d.is_empty()) else {
        eprintln!("skipped: WFSCAN_CACHE_DIR is not set");
        return;
    };
    let Ok(oodle) = wfscan_core::Oodle::load(None) else {
        eprintln!("skipped: no Oodle library");
        return;
    };
    let oodle: &dyn OodleDecoder = &oodle;
    let dir = Path::new(&dir);
    let cache = Cache::open(dir).unwrap();
    // The bundled header sets must be the ones this game has.
    let tables = VorbisTables::bundled();
    if let Some(exe) = VorbisTables::find_exe(dir) {
        let in_game = VorbisTables::load(&exe).unwrap();
        let known = in_game.sets.iter().filter(|s| tables.sets.contains(s)).count();
        eprintln!("{}: {} Vorbis header sets, {known} of them bundled", exe.display(), in_game.sets.len());
        assert!(!in_game.sets.is_empty() && known == in_game.sets.len(), "the game has header sets wfscan doesn't bundle: run tools/vorbis-headers --check");
    }

    let (manifest, _) = index(&cache, Some(oodle), &IndexOptions::default()).unwrap();
    let classifier = manifest.classifier();
    let full = std::env::var_os("WFSCAN_FULL").is_some();
    let folders = ["/Lotus/Sounds/Warframes/Excalibur/", "/Lotus/Music/Cinematics/", "/Lotus/Sounds/Dialog/1999TCDemo/", "/Lotus/Sounds/Enemies/Duviri/", "/EE/Sounds/"];
    let files: Vec<_> = manifest
        .files
        .iter()
        .filter(|f| !f.stale && classifier.kind(f) == Some(Kind::Audio) && (full || folders.iter().any(|folder| f.path.starts_with(folder))))
        .collect();

    // Per file: its codec and how much of it clips, or why it didn't decode.
    let results: Vec<(String, Result<f64, String>)> = files
        .par_iter()
        .map(|f| {
            let outcome = with_input(&cache, &f.package, &f.path, Some(oodle), |input| {
                let sound = audio::read(input).map_err(|e| match e {
                    ExportError::Unsupported(r) | ExportError::Failed(r) => r,
                })?;
                let pcm = audio::decode(&sound, None)?;
                if pcm.frames() != sound.samples as usize {
                    return Err(format!("{} frames decoded, the header says {}", pcm.frames(), sound.samples));
                }
                let clipped = pcm.samples.iter().filter(|&&s| s == i16::MAX || s == i16::MIN).count() as f64 / pcm.samples.len().max(1) as f64;
                Ok((sound.codec.name().to_string(), clipped))
            });
            match outcome {
                Ok(Ok((codec, clipped))) => (codec, Ok(clipped)),
                Ok(Err(e)) => ("failed".into(), Err(format!("{}: {e}", f.path))),
                Err(e) => ("unreadable".into(), Err(format!("{}: {e}", f.path))),
            }
        })
        .collect();

    let mut counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut errors: BTreeMap<String, (usize, &str)> = BTreeMap::new();
    for (codec, result) in &results {
        let entry = counts.entry(codec).or_default();
        entry.0 += 1;
        match result {
            Ok(clipped) => entry.1 += usize::from(*clipped > 0.02),
            Err(e) => {
                // Group by the reason without its path and numbers.
                let reason = e.split_once(": ").map_or(e.as_str(), |(_, reason)| reason);
                let key: String = reason.chars().filter(|c| !c.is_ascii_digit()).collect();
                errors.entry(key).or_insert((0, e)).0 += 1;
            }
        }
    }
    for (codec, (files, noisy)) in &counts {
        eprintln!("{codec}: {files} files, {noisy} clip more than 2% of their samples");
    }
    for (count, sample) in errors.values() {
        eprintln!("{count} x {sample}");
    }
    let failed = counts.get("failed").map_or(0, |c| c.0) + counts.get("unreadable").map_or(0, |c| c.0);
    let noisy: usize = counts.values().map(|c| c.1).sum();
    assert!(results.len() > 500, "{} sounds", results.len());
    assert!(failed * 100 <= results.len(), "{failed} of {} sounds didn't decode", results.len());
    assert!(noisy * 50 <= results.len(), "{noisy} of {} sounds are mostly clipping", results.len());
    for codec in ["Vorbis", "Opus"] {
        assert!(counts.get(codec).is_some_and(|c| c.0 > 20), "{codec} sounds were decoded");
    }
}

/// Real sounds exported and read back: the Ogg Vorbis files with lewton's own Ogg
/// reader, the Ogg Opus files page by page, both against decoding the sound directly.
/// Needs `WFSCAN_CACHE_DIR`, `WFSCAN_OODLE_DLL` and the `audio` feature.
#[cfg(feature = "audio")]
#[test]
fn real_sounds_export_and_read_back() {
    use std::path::{Path, PathBuf};

    use wfscan_core::OodleDecoder;

    let Some(dir) = std::env::var_os("WFSCAN_CACHE_DIR").filter(|d| !d.is_empty()) else {
        eprintln!("skipped: WFSCAN_CACHE_DIR is not set");
        return;
    };
    let Ok(oodle) = wfscan_core::Oodle::load(None) else {
        eprintln!("skipped: no Oodle library");
        return;
    };
    let oodle: &dyn OodleDecoder = &oodle;
    let dir = Path::new(&dir);
    let cache = Cache::open(dir).unwrap();
    let (mut manifest, _) = index(&cache, Some(oodle), &IndexOptions::default()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    // The last folder's dialogue is 24 kHz Opus.
    let folders = ["/Lotus/Sounds/Warframes/Excalibur/", "/Lotus/Music/AmbulasReborn/", "/Lotus/Sounds/Dialog/1999TCDemo/", "/Lotus/Sounds/Dialog/MegaPrimeVault/"];

    let mut sizes = [0u64; 2];
    for (i, format) in [AudioOutput::Native, AudioOutput::Wav].into_iter().enumerate() {
        let exporters = Exporters::builtin(&FormatOptions { audio: format, ..Default::default() });
        for folder in folders {
            let opts = ExportOptions { path: Some(folder.into()), kinds: vec![Kind::Audio], ..Default::default() };
            let stats = export(&cache, &mut manifest, None, &exporters, Some(oodle), &tmp.path().join(format.name()), &opts).unwrap();
            eprintln!("{folder} as {format}: {} ok, {} unsupported, {} failed, {:.1} MiB", stats.ok, stats.unsupported, stats.failed, stats.bytes as f64 / 1048576.0);
            assert!(stats.ok > 0 && stats.failed == 0 && stats.unsupported == 0, "{folder}: {stats:?}");
            sizes[i] += stats.bytes;
        }
    }
    assert!(sizes[1] > sizes[0] * 3, "decoded WAV is several times the size of the compressed files");

    let file_at = |format: AudioOutput, internal: &str| -> PathBuf { internal.split('/').filter(|p| !p.is_empty()).fold(tmp.path().join(format.name()), |dir, part| dir.join(part)) };
    let (mut vorbis, mut opus) = (0, 0);
    for file in manifest.files.iter().filter(|f| !f.stale && folders.iter().any(|folder| f.path.starts_with(folder))) {
        let Some(record) = file.export.as_ref().filter(|r| r.status == Status::Ok) else { continue };
        let sound = with_input(&cache, &file.package, &file.path, Some(oodle), audio::read).unwrap().ok().unwrap();
        let direct = audio::decode(&sound, None).unwrap();
        // The last export was the WAV one; the native file sits beside it under its own name.
        let wav = std::fs::read(file_at(AudioOutput::Wav, &record.outputs[0])).unwrap();
        let from_wav: Vec<i16> = wav[44..].as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        assert_eq!(from_wav, direct.samples, "{}", file.path);

        match sound.codec {
            Codec::Vorbis => {
                let bytes = std::fs::read(file_at(AudioOutput::Native, &audio::output_path(&file.path, "ogg"))).unwrap();
                let mut reader = lewton::inside_ogg::OggStreamReader::new(std::io::Cursor::new(&bytes)).unwrap_or_else(|e| panic!("{}: {e}", file.path));
                assert_eq!((reader.ident_hdr.audio_channels, reader.ident_hdr.audio_sample_rate), (sound.channels, sound.sample_rate));
                let mut samples = Vec::new();
                while let Some(packet) = reader.read_dec_packet_itl().unwrap_or_else(|e| panic!("{}: {e}", file.path)) {
                    samples.extend(packet);
                }
                // A player stops at the last page's position: the sound's length.
                let (pages, _) = ogg::parse(&bytes).expect("well-formed pages");
                assert_eq!(pages.last().unwrap().1, u64::from(sound.samples), "{}", file.path);
                samples.truncate(sound.samples as usize * usize::from(sound.channels));
                assert_eq!(samples, direct.samples, "{}", file.path);
                vorbis += 1;
            }
            Codec::Opus => {
                let bytes = std::fs::read(file_at(AudioOutput::Native, &audio::output_path(&file.path, "opus"))).unwrap();
                let (pages, packets) = ogg::parse(&bytes).expect("well-formed pages");
                let head = &packets[0];
                assert_eq!((&head[..8], head[8], head[9]), (&b"OpusHead"[..], 1, sound.channels));
                assert_eq!(&packets[1][..8], b"OpusTags");
                // The file counts at 48 kHz; decode at the sound's own rate to compare.
                let scale = 48000 / sound.sample_rate as usize;
                let pre_skip = usize::from(u16::from_le_bytes([head[10], head[11]])) / scale;
                assert_eq!(u32::from_le_bytes(head[12..16].try_into().unwrap()), sound.sample_rate);
                assert_eq!(pages.last().unwrap().1, ((pre_skip + sound.samples as usize) * scale) as u64, "{}", file.path);
                let channels = usize::from(sound.channels);
                let layout = if channels == 1 { ropus::Channels::Mono } else { ropus::Channels::Stereo };
                let mut decoder = ropus::Decoder::new(sound.sample_rate, layout).unwrap();
                let mut block = vec![0i16; 5760 * channels];
                let mut samples = Vec::new();
                for packet in &packets[2..] {
                    let frames = decoder.decode(packet, &mut block, ropus::DecodeMode::Normal).unwrap();
                    samples.extend_from_slice(&block[..frames * channels]);
                }
                samples.drain(..pre_skip * channels);
                samples.truncate(sound.samples as usize * channels);
                assert_eq!(samples, direct.samples, "{}", file.path);
                opus += 1;
            }
            Codec::Pcm => {}
        }
    }
    eprintln!("{vorbis} Ogg Vorbis and {opus} Ogg Opus files read back the same as the sounds decode");
    assert!(vorbis > 20 && opus > 5);
}
