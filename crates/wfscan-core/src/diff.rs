//! What changed between two manifests of the same cache, such as before and after a game
//! update: files added, removed, and changed.
//!
//! Only live files count. A file is the same if [`FileEntry::same_content`] says so.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::cache::package::Category;
use crate::manifest::{Classifier, FileEntry, Manifest};
use crate::types::Kind;

#[derive(Debug, Default)]
pub struct Diff<'a> {
    /// In `new` only.
    pub added: Vec<&'a FileEntry>,
    /// In `old` only.
    pub removed: Vec<&'a FileEntry>,
    /// In both, with different contents: (old, new).
    pub changed: Vec<(&'a FileEntry, &'a FileEntry)>,
    pub unchanged: u64,
    /// Totals per package category and type, largest first.
    pub summary: Vec<DiffRow>,
}

/// How many files of one type changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffRow {
    pub category: Category,
    /// `None` for files without a common header.
    #[serde(rename = "type")]
    pub type_id: Option<u32>,
    pub kind: Option<Kind>,
    pub added: u64,
    pub removed: u64,
    pub changed: u64,
}

/// Compare two manifests. Files come out in the order their manifest lists them.
pub fn diff<'a>(old: &'a Manifest, new: &'a Manifest) -> Diff<'a> {
    let key = |f: &'a FileEntry| (f.package.as_str(), f.path.as_str());
    let live = |m: &'a Manifest| m.files.iter().filter(|f| !f.stale);
    let before: HashMap<(&str, &str), &FileEntry> = live(old).map(|f| (key(f), f)).collect();
    let after: HashMap<(&str, &str), &FileEntry> = live(new).map(|f| (key(f), f)).collect();

    let mut diff = Diff::default();
    for f in live(new) {
        match before.get(&key(f)) {
            None => diff.added.push(f),
            Some(o) if !o.same_content(f) => diff.changed.push((o, f)),
            Some(_) => diff.unchanged += 1,
        }
    }
    diff.removed = live(old).filter(|f| !after.contains_key(&key(f))).collect();

    let (old_class, new_class) = (old.classifier(), new.classifier());
    let mut rows = Rows::new();
    for f in &diff.added {
        row(&mut rows, &new_class, f).added += 1;
    }
    for (_, f) in &diff.changed {
        row(&mut rows, &new_class, f).changed += 1;
    }
    for f in &diff.removed {
        row(&mut rows, &old_class, f).removed += 1;
    }
    diff.summary = rows.into_values().collect();
    diff.summary.sort_by_key(|r| std::cmp::Reverse(r.added + r.removed + r.changed));
    diff
}

type Rows = BTreeMap<(Category, Option<u32>), DiffRow>;

/// The summary row for `file`'s category and type.
fn row<'r>(rows: &'r mut Rows, classifier: &Classifier, file: &FileEntry) -> &'r mut DiffRow {
    let category = classifier.category(file);
    let type_id = file.header.as_ref().map(|h| h.type_id);
    let kind = classifier.kind(file);
    rows.entry((category, type_id)).or_insert(DiffRow { category, type_id, kind, added: 0, removed: 0, changed: 0 })
}
