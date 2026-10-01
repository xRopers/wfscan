//! Ogg files for Vorbis and Opus sounds: the game's own packets, not re-encoded, behind
//! the header packets a player needs.
//!
//! - Vorbis (`.ogg`): the identification, comment and setup packets from the game's
//!   executable ([`super::vorbis`]), then the sound's packets. Each page's position is
//!   the sample position the game's packet table gives for the last packet that ends on
//!   it; the last one is the sound's length, so a player stops there.
//! - Opus (`.opus`): an `OpusHead` and an `OpusTags` packet written here, then the
//!   packets. Positions count 48 kHz samples from the first decoded one, lead-in
//!   included, whatever the sound's own sample rate; the last is lead-in plus length.
//!
//! Format: RFC 3533 (Ogg), the Vorbis I specification's Ogg mapping, RFC 7845 (Ogg Opus).

use super::model::{Audio, opus_packet_samples};
use super::vorbis::VorbisHeaders;

/// A page is closed once it holds this many bytes, so players can seek finely.
const PAGE_BYTES: usize = 4096;
/// Every stream wfscan writes has this serial number: one stream per file needs no more.
const SERIAL: u32 = 0x7766_7363;

/// Ogg's checksum: CRC-32 with polynomial 0x04c11db7, most significant bit first, no
/// reflection, starting from zero.
const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 0x8000_0000 != 0 { (value << 1) ^ 0x04c1_1db7 } else { value << 1 };
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

pub fn crc(data: &[u8]) -> u32 {
    data.iter().fold(0, |crc, &byte| (crc << 8) ^ CRC_TABLE[((crc >> 24) as u8 ^ byte) as usize])
}

/// Packs packets into the pages of one Ogg stream.
pub struct OggWriter {
    out: Vec<u8>,
    sequence: u32,
    /// The page being filled: its lacing values and its data.
    segments: Vec<u8>,
    body: Vec<u8>,
    /// The position of the last packet that ends on the page being filled.
    position: Option<u64>,
    /// The page being filled starts in the middle of a packet.
    continued: bool,
}

impl Default for OggWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl OggWriter {
    pub fn new() -> Self {
        Self { out: Vec::new(), sequence: 0, segments: Vec::new(), body: Vec::new(), position: None, continued: false }
    }

    /// Add a packet that ends at `position`. A packet too large for the page being
    /// filled runs on into the next.
    pub fn packet(&mut self, data: &[u8], position: u64) {
        let needed = data.len() / 255 + 1;
        if !self.segments.is_empty() && (self.body.len() >= PAGE_BYTES || self.segments.len() + needed > 255) {
            self.page(false);
        }
        let mut rest = data;
        loop {
            if self.segments.len() == 255 {
                self.page(false);
                self.continued = true;
            }
            let take = rest.len().min(255);
            self.segments.push(take as u8);
            self.body.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if take < 255 {
                break;
            }
        }
        self.position = Some(position);
    }

    /// Close the page being filled, so the next packet starts a new one.
    pub fn flush(&mut self) {
        if !self.segments.is_empty() {
            self.page(false);
        }
    }

    /// Close the last page and return the file.
    pub fn finish(mut self) -> Vec<u8> {
        self.page(true);
        self.out
    }

    fn page(&mut self, last: bool) {
        let start = self.out.len();
        let flags = u8::from(self.continued) | if self.sequence == 0 { 2 } else { 0 } | if last { 4 } else { 0 };
        self.out.extend_from_slice(b"OggS\0");
        self.out.push(flags);
        // No packet ends on this page: the position is "none".
        self.out.extend_from_slice(&self.position.unwrap_or(u64::MAX).to_le_bytes());
        self.out.extend_from_slice(&SERIAL.to_le_bytes());
        self.out.extend_from_slice(&self.sequence.to_le_bytes());
        self.out.extend_from_slice(&[0; 4]);
        self.out.push(self.segments.len() as u8);
        self.out.extend_from_slice(&self.segments);
        self.out.extend_from_slice(&self.body);
        let checksum = crc(&self.out[start..]);
        self.out[start + 22..start + 26].copy_from_slice(&checksum.to_le_bytes());
        self.sequence += 1;
        self.segments.clear();
        self.body.clear();
        self.position = None;
        self.continued = false;
    }
}

/// A Vorbis sound as an Ogg Vorbis file, with the header packets of `headers`.
pub fn vorbis(audio: &Audio, headers: &VorbisHeaders) -> Vec<u8> {
    let mut ogg = OggWriter::new();
    ogg.packet(&headers.ident, 0);
    ogg.flush();
    ogg.packet(&headers.comment, 0);
    ogg.packet(&headers.setup, 0);
    ogg.flush();
    for (i, packet) in audio.packets.iter().enumerate() {
        let position = audio.positions.get(i).copied().unwrap_or(u64::from(audio.samples));
        ogg.packet(&audio.data[packet.clone()], position.min(u64::from(audio.samples)));
    }
    ogg.finish()
}

/// An Opus sound as an Ogg Opus file.
pub fn opus(audio: &Audio) -> Vec<u8> {
    // Ogg Opus counts at 48 kHz.
    let to_48k = |samples: u64| samples * 48000 / u64::from(audio.sample_rate.max(1));
    let pre_skip = to_48k(audio.opus_pre_skip() as u64);
    let mut head = b"OpusHead\x01".to_vec();
    head.push(audio.channels);
    head.extend_from_slice(&(pre_skip as u16).to_le_bytes());
    head.extend_from_slice(&audio.sample_rate.to_le_bytes());
    // No gain; channel mapping family 0 (mono or stereo).
    head.extend_from_slice(&[0, 0, 0]);
    let vendor = b"wfscan";
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes());

    let mut ogg = OggWriter::new();
    ogg.packet(&head, 0);
    ogg.flush();
    ogg.packet(&tags, 0);
    ogg.flush();
    let end = pre_skip + to_48k(u64::from(audio.samples));
    let mut decoded = 0;
    for (i, packet) in audio.packets.iter().enumerate() {
        let packet = &audio.data[packet.clone()];
        decoded += u64::from(opus_packet_samples(packet));
        let last = i + 1 == audio.packets.len();
        ogg.packet(packet, if last { end.min(decoded) } else { decoded });
    }
    ogg.finish()
}

/// A page's flags (1 continues a packet, 2 first page, 4 last page), position and
/// sequence number.
pub type Page = (u8, u64, u32);

/// The pages of an Ogg file, for checking what was written: each page's flags, position
/// and sequence number, and the packets of the whole stream. `None` if a page is
/// malformed or its checksum is wrong.
pub fn parse(file: &[u8]) -> Option<(Vec<Page>, Vec<Vec<u8>>)> {
    let (mut pages, mut packets) = (Vec::new(), Vec::new());
    let mut packet: Vec<u8> = Vec::new();
    let mut at = 0;
    while at < file.len() {
        let header = file.get(at..at + 27)?;
        if &header[..5] != b"OggS\0" {
            return None;
        }
        let segments = file.get(at + 27..at + 27 + usize::from(header[26]))?;
        let body_len: usize = segments.iter().map(|&s| usize::from(s)).sum();
        let end = at + 27 + segments.len() + body_len;
        let mut page = file.get(at..end)?.to_vec();
        let stored = u32::from_le_bytes(page[22..26].try_into().ok()?);
        page[22..26].fill(0);
        if crc(&page) != stored {
            return None;
        }
        pages.push((header[5], u64::from_le_bytes(header[6..14].try_into().ok()?), u32::from_le_bytes(header[18..22].try_into().ok()?)));
        let mut body = &file[at + 27 + segments.len()..end];
        for &segment in segments {
            packet.extend_from_slice(&body[..usize::from(segment)]);
            body = &body[usize::from(segment)..];
            if segment < 255 {
                packets.push(std::mem::take(&mut packet));
            }
        }
        at = end;
    }
    packet.is_empty().then_some((pages, packets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_is_oggs() {
        // The CRC of a single 0x01 byte is the polynomial itself.
        assert_eq!(crc(&[1]), 0x04c1_1db7);
        assert_eq!(crc(&[]), 0);
        // The check value of this CRC (CRC-32/MPEG-2 without the initial ones).
        assert_eq!(crc(b"123456789"), 0x89a1_897f);
    }

    #[test]
    fn packets_come_back_out_of_the_pages() {
        let big: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let exact = vec![7u8; 510];
        let mut ogg = OggWriter::new();
        ogg.packet(b"first", 0);
        ogg.flush();
        ogg.packet(&exact, 100);
        ogg.packet(&[], 150);
        ogg.packet(&big, 200);
        ogg.packet(b"last", 300);
        let file = ogg.finish();
        let (pages, packets) = parse(&file).expect("well-formed pages");
        assert_eq!(packets, [b"first".to_vec(), exact, Vec::new(), big, b"last".to_vec()]);
        // First page alone, flagged as the beginning; sequence numbers count up; the big
        // packet fills a page (flag 1 on the next: continued) on which nothing ends.
        assert_eq!(pages.iter().map(|p| p.2).collect::<Vec<_>>(), (0..pages.len() as u32).collect::<Vec<_>>());
        assert_eq!((pages[0].0, pages[0].1), (2, 0));
        assert_eq!(pages.last().unwrap().0 & 4, 4, "the last page says so");
        assert!(pages.iter().any(|p| p.1 == u64::MAX) && pages.iter().any(|p| p.0 & 1 == 1));
        assert_eq!(pages.last().unwrap().1, 300);

        // A flipped byte is caught.
        let mut broken = file.clone();
        broken[40] ^= 1;
        assert!(parse(&broken).is_none());
    }

    #[test]
    fn pages_close_at_about_four_kilobytes() {
        let mut ogg = OggWriter::new();
        for i in 0..100u64 {
            ogg.packet(&[0xab; 300], i);
        }
        let (pages, packets) = parse(&ogg.finish()).unwrap();
        assert_eq!(packets.len(), 100);
        // 14 packets of 300 bytes pass 4096; the page closes before the 15th.
        assert_eq!(pages.len(), 100usize.div_ceil(14));
        assert_eq!(pages[0].1, 13);
    }
}
