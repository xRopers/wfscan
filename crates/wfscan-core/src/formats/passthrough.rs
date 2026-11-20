//! Files whose data is already a standard format, or plain text: they are copied out
//! after checking they are what they claim to be. One exporter per kind, all here,
//! because each is a few lines.
//!
//! Where the facts come from (all from the November 2026 cache, looked at with
//! `wfscan mcp`; none of these types are exported by Warframe-Exporter):
//! - Videos used to be here; they have their own exporter now (`formats::video`).
//! - **Flash** (`AnimRetarget` 9, `Misc` 10, 508 `.swf` files): B is a whole SWF. `FWS`
//!   is an uncompressed SWF whose u32 at offset 4 is the file's length; `CWS` (zlib) and
//!   `ZWS` (LZMA) store the uncompressed length there instead, so only the magic is checked.
//! - **Script** (`Font` 110 and 120, 5,478 `.lua` files): B is compiled bytecode in a
//!   Luau-like layout: a version byte, a types-version byte, then a string table (a
//!   varint count, then each string as a varint length and its bytes). Written as is
//!   (`.luac`); the string table is checked so a file of another layout fails instead of
//!   going out as junk. Not decompiled.
//! - **Sound bank** (`Misc` 6, 44,417 extensionless files under `Sounds`): only an H part,
//!   and the whole bank (default settings, each sample with its gain, pitch and play mode)
//!   is the common header's attribute text. A few bytes follow the header (in editor
//!   banks, a build error message), not exported. Written as `.txt`. Of the 44,417,
//!   1,296 are empty 32-byte stubs (no sources, no text) and 30 have a B part
//!   (animation blend sets and the like, not banks): both are left unsupported.

use crate::cache::package::Part;
use crate::export::{ExportError, ExportInput, Exporter, Output};
use crate::types::Kind;

/// Copies one kind of file out as it is.
pub struct PassthroughExporter {
    pub kind: Kind,
}

impl PassthroughExporter {
    /// The kinds this exporter handles.
    pub const KINDS: [Kind; 3] = [Kind::Flash, Kind::Script, Kind::SoundBank];
}

impl Exporter for PassthroughExporter {
    fn name(&self) -> &'static str {
        self.kind.name()
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> Kind {
        self.kind
    }

    fn export(&self, input: &ExportInput) -> Result<Vec<Output>, ExportError> {
        let (data, extension, known): (Vec<u8>, &str, &[&str]) = match self.kind {
            Kind::Flash => (swf(input)?, "swf", &["swf"]),
            Kind::Script => (script(input)?, "luac", &["lua", "luac"]),
            Kind::SoundBank => {
                // 30 files of the type carry data in B (animation blend sets, a few
                // `_data.png`); the text alone would leave that out.
                if input.has(Part::B) || input.has(Part::F) {
                    return Err(ExportError::unsupported("has data in B or F, so it isn't a plain sound bank"));
                }
                let text = &input.header.attributes;
                if text.trim().is_empty() {
                    return Err(ExportError::unsupported("the sound bank has no attribute text"));
                }
                (text.as_bytes().to_vec(), "txt", &[])
            }
            other => return Err(ExportError::unsupported(format!("no passthrough rule for {other}"))),
        };
        Ok(vec![Output { path: output_path(input.path, extension, known), data }])
    }
}

/// `path` with `extension`: replacing its own if that is one of `known` (any case),
/// else added to the end.
pub fn output_path(path: &str, extension: &str, known: &[&str]) -> String {
    let name_start = path.rfind('/').map_or(0, |i| i + 1);
    if let Some(dot) = path[name_start..].rfind('.') {
        let own = &path[name_start + dot + 1..];
        if known.iter().any(|k| k.eq_ignore_ascii_case(own)) {
            return format!("{}.{extension}", &path[..name_start + dot]);
        }
    }
    format!("{path}.{extension}")
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(offset..offset + 4)?.try_into().ok()?))
}

fn swf(input: &ExportInput) -> Result<Vec<u8>, ExportError> {
    let data = input.read(Part::B)?;
    match data.get(..3) {
        Some(b"FWS") => {
            let stated = u32_at(&data, 4).map(|n| n as usize);
            if stated != Some(data.len()) {
                return Err(ExportError::failed(format!("the SWF header says {stated:?} bytes, B holds {}", data.len())));
            }
        }
        Some(b"CWS" | b"ZWS") => {}
        _ => return Err(ExportError::unsupported("B doesn't start with FWS, CWS or ZWS")),
    }
    Ok(data)
}

fn script(input: &ExportInput) -> Result<Vec<u8>, ExportError> {
    let data = input.read(Part::B)?;
    check_string_table(&data).map_err(|e| ExportError::failed(format!("not the expected bytecode layout: {e}")))?;
    Ok(data)
}

/// Read a LEB128 varint at `*at`.
fn varint(data: &[u8], at: &mut usize) -> Option<usize> {
    let mut value = 0usize;
    for shift in (0..35).step_by(7) {
        let byte = *data.get(*at)?;
        *at += 1;
        value |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// Check the version bytes and that every string of the table fits.
fn check_string_table(data: &[u8]) -> Result<(), String> {
    if data.len() < 3 {
        return Err("too short".into());
    }
    let mut at = 2;
    let count = varint(data, &mut at).ok_or("the string count runs off the end")?;
    for i in 0..count {
        let len = varint(data, &mut at).ok_or_else(|| format!("string {i}'s length runs off the end"))?;
        at = at.checked_add(len).filter(|&end| end <= data.len()).ok_or_else(|| format!("string {i} runs off the end"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_names() {
        assert_eq!(output_path("/A/Intro.bk2", "bk2", &["bk2", "bik"]), "/A/Intro.bk2");
        assert_eq!(output_path("/A/Old.BIK", "bik", &["bk2", "bik"]), "/A/Old.bik");
        assert_eq!(output_path("/A/Menu.lua", "luac", &["lua", "luac"]), "/A/Menu.luac");
        assert_eq!(output_path("/A/Sounds/Hit", "txt", &[]), "/A/Sounds/Hit.txt");
        assert_eq!(output_path("/A.b/Hit", "txt", &["b"]), "/A.b/Hit.txt", "a dot in a folder name isn't an extension");
    }

    #[test]
    fn string_tables() {
        assert!(check_string_table(&[6, 3, 2, 1, b'a', 2, b'b', b'c', 0xff]).is_ok());
        assert!(check_string_table(&[6, 3, 0]).is_ok());
        assert!(check_string_table(&[6, 3, 2, 1, b'a', 9, b'b']).unwrap_err().contains("string 1"));
        assert!(check_string_table(&[6, 3, 0x80]).is_err());
        assert!(check_string_table(&[6]).is_err());
    }
}
