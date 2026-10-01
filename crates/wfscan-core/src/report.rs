//! Coverage: for every file type in a manifest, how many files there are, whether wfscan
//! knows what they are, and how their reads and exports ended.
//!
//! This is where a format broken by a game update shows up: as one row with a failure
//! count and one line in the error list, not as a crash somewhere in a long run.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::cache::package::Category;
use crate::manifest::{Manifest, Status};
use crate::types::Kind;

/// One file type within one package category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TypeRow {
    pub category: Category,
    /// `None` for files without a common header.
    #[serde(rename = "type")]
    pub type_id: Option<u32>,
    /// `None` if no line of the type table covers this number.
    pub kind: Option<Kind>,
    pub files: u64,
    /// Decompressed bytes over all parts.
    pub size: u64,
    /// Files with a part `wfscan index` couldn't read.
    pub unreadable: u64,
    pub ok: u64,
    pub failed: u64,
    pub unsupported: u64,
    pub skipped: u64,
    /// One of the files, to look at.
    pub sample: String,
}

impl TypeRow {
    /// Files no export has touched.
    pub fn not_exported(&self) -> u64 {
        self.files - self.ok - self.failed - self.unsupported - self.skipped
    }
}

/// Files of one type that failed the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorRow {
    pub category: Category,
    #[serde(rename = "type")]
    pub type_id: Option<u32>,
    pub kind: Option<Kind>,
    /// The message with every number replaced by `#`, so offsets don't split a group.
    pub message: String,
    pub files: u64,
    pub sample: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// By category, then most files first.
    pub types: Vec<TypeRow>,
    /// Read errors and export failures, most files first.
    pub errors: Vec<ErrorRow>,
}

/// `block at 0x1f: 12 bytes` and `block at 0x20: 7 bytes` are the same failure.
fn without_numbers(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut chars = message.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            // A whole decimal or 0x... number becomes one '#'.
            if c == '0' && chars.peek() == Some(&'x') {
                chars.next();
            }
            while chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                chars.next();
            }
            out.push('#');
        } else {
            out.push(c);
        }
    }
    out
}

/// Summarise the live files of a manifest by type.
pub fn report(manifest: &Manifest) -> Report {
    let classifier = manifest.classifier();
    let mut types: BTreeMap<(Category, Option<u32>), TypeRow> = BTreeMap::new();
    let mut errors: BTreeMap<(Category, Option<u32>, String), ErrorRow> = BTreeMap::new();
    for file in manifest.files.iter().filter(|f| !f.stale) {
        let category = classifier.category(file);
        let type_id = file.header.as_ref().map(|h| h.type_id);
        let kind = classifier.kind(file);
        let row = types.entry((category, type_id)).or_insert_with(|| TypeRow {
            category,
            type_id,
            kind,
            files: 0,
            size: 0,
            unreadable: 0,
            ok: 0,
            failed: 0,
            unsupported: 0,
            skipped: 0,
            sample: file.path.clone(),
        });
        row.files += 1;
        row.size += file.size();
        row.unreadable += u64::from(file.error.is_some());
        match file.export.as_ref().map(|e| e.status) {
            Some(Status::Ok) => row.ok += 1,
            Some(Status::Failed) => row.failed += 1,
            Some(Status::Unsupported) => row.unsupported += 1,
            Some(Status::Skipped) => row.skipped += 1,
            None => {}
        }
        let export_failure = file.export.as_ref().filter(|e| e.status == Status::Failed).and_then(|e| e.reason.as_deref());
        for message in file.error.as_deref().into_iter().chain(export_failure) {
            let message = without_numbers(message);
            let row = errors.entry((category, type_id, message.clone())).or_insert_with(|| ErrorRow {
                category,
                type_id,
                kind,
                message,
                files: 0,
                sample: file.path.clone(),
            });
            row.files += 1;
        }
    }
    let mut types: Vec<TypeRow> = types.into_values().collect();
    types.sort_by(|a, b| a.category.cmp(&b.category).then(b.files.cmp(&a.files)));
    let mut errors: Vec<ErrorRow> = errors.into_values().collect();
    errors.sort_by_key(|e| std::cmp::Reverse(e.files));
    Report { types, errors }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_blanked() {
        assert_eq!(without_numbers("B: block at 0x1f40: 12 bytes run past"), "B: block at #: # bytes run past");
        assert_eq!(without_numbers("no reader for type 210"), "no reader for type #");
        assert_eq!(without_numbers("nothing to blank"), "nothing to blank");
        assert_eq!(without_numbers("0"), "#");
    }
}
