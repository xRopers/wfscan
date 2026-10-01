//! Generates the Vorbis header sets wfscan bundles, with libvorbis 1.3.7's encoder.
//!
//! Warframe stores its Vorbis sounds without the three header packets, which it keeps in
//! its executable. Those packets are exactly what libvorbis 1.3.7 writes for
//! `vorbis_encode_init_vbr` at the channel counts, sample rates and qualities below, so
//! wfscan generates them from libvorbis instead of taking anything from the game.
//!
//! ```text
//! cargo run --release -- ../../crates/wfscan-core/data/vorbis_headers.bin [--check GAME.exe]
//! ```
//!
//! `--check` compares the generated sets with the ones in a game executable and fails if
//! the game has a set this list doesn't produce (a game update may add one: find its
//! quality by trying values and add a line to `SETS`).

use std::mem::zeroed;
use std::process::ExitCode;

use wfscan_core::formats::audio::VorbisTables;

/// Channels, sample rate and encoder quality of each set, in the order the game's
/// executable holds them (September 2026).
const SETS: [(i32, i32, f32); 12] = [
    (1, 32000, -0.1),
    (2, 32000, -0.1),
    (1, 32000, 0.45),
    (2, 32000, 0.45),
    (1, 44100, 0.725),
    (2, 44100, 0.725),
    (1, 44100, 0.78),
    (2, 44100, 0.78),
    (1, 44100, 0.835),
    (2, 44100, 0.835),
    (1, 44100, 0.89),
    (2, 44100, 0.89),
];

unsafe extern "C" {
    fn vorbis_encode_init_vbr(vi: *mut vorbis_sys::vorbis_info, channels: libc::c_long, rate: libc::c_long, quality: f32) -> libc::c_int;
}

/// The identification, comment and setup packets libvorbis writes for these settings.
fn headers(channels: i32, rate: i32, quality: f32) -> Option<[Vec<u8>; 3]> {
    // SAFETY: the libvorbis structs are plain C structs that its init functions fill in;
    // the packets point into memory libvorbis owns until the clear calls, and are copied
    // before those.
    unsafe {
        let mut vi: vorbis_sys::vorbis_info = zeroed();
        vorbis_sys::vorbis_info_init(&mut vi);
        if vorbis_encode_init_vbr(&mut vi, channels as _, rate as _, quality) != 0 {
            vorbis_sys::vorbis_info_clear(&mut vi);
            return None;
        }
        let mut vd: vorbis_sys::vorbis_dsp_state = zeroed();
        vorbis_sys::vorbis_analysis_init(&mut vd, &mut vi);
        let mut vc: vorbis_sys::vorbis_comment = zeroed();
        vorbis_sys::vorbis_comment_init(&mut vc);
        let mut packets: [ogg_sys::ogg_packet; 3] = zeroed();
        let [ident, comment, setup] = &mut packets;
        vorbis_sys::vorbis_analysis_headerout(&mut vd, &mut vc, ident, comment, setup);
        let out = packets.each_ref().map(|p| std::slice::from_raw_parts(p.packet, p.bytes as usize).to_vec());
        vorbis_sys::vorbis_dsp_clear(&mut vd);
        vorbis_sys::vorbis_comment_clear(&mut vc);
        vorbis_sys::vorbis_info_clear(&mut vi);
        Some(out)
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(out) = args.first() else {
        eprintln!("usage: vorbis-headers OUTPUT [--check GAME.exe]");
        return ExitCode::FAILURE;
    };

    // The file: "WFVORBIS", a u32 count, then per set the quality x 1000 as an i32 and
    // the three packets, each behind a u32 length.
    let mut file = b"WFVORBIS".to_vec();
    file.extend_from_slice(&(SETS.len() as u32).to_le_bytes());
    let mut generated = Vec::new();
    for (channels, rate, quality) in SETS {
        let Some(packets) = headers(channels, rate, quality) else {
            eprintln!("libvorbis refused {channels} channels at {rate} Hz, quality {quality}");
            return ExitCode::FAILURE;
        };
        file.extend_from_slice(&((quality * 1000.0).round() as i32).to_le_bytes());
        for packet in &packets {
            file.extend_from_slice(&(packet.len() as u32).to_le_bytes());
            file.extend_from_slice(packet);
        }
        generated.push(packets);
    }
    if let Err(e) = std::fs::write(out, &file) {
        eprintln!("{out}: {e}");
        return ExitCode::FAILURE;
    }
    println!("wrote {} sets, {} bytes, to {out}", SETS.len(), file.len());

    if let Some(exe) = args.iter().position(|a| a == "--check").and_then(|i| args.get(i + 1)) {
        let tables = match VorbisTables::load(std::path::Path::new(exe)) {
            Ok(tables) => tables,
            Err(e) => {
                eprintln!("{exe}: {e}");
                return ExitCode::FAILURE;
            }
        };
        let mut missing = 0;
        for set in &tables.sets {
            let same = generated.iter().any(|[ident, comment, setup]| *ident == set.ident && *comment == set.comment && *setup == set.setup);
            println!("{} ch {} Hz {} bps: {}", set.channels, set.sample_rate, set.bitrate, if same { "identical to a generated set" } else { "NOT generated" });
            missing += usize::from(!same);
        }
        println!("{} sets in {exe}, {missing} not generated", tables.sets.len());
        if missing > 0 || tables.sets.len() != generated.len() {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
