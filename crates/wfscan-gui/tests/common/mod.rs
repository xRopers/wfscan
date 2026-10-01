//! A small cache for the GUI tests: the texture, model and sound fixtures in one folder, with
//! LZF in place of the stand-in Oodle blocks so nothing needs an Oodle library.

#![allow(dead_code)]

use std::path::PathBuf;

use wfscan_fixtures as fx;

pub struct Input {
    pub dir: tempfile::TempDir,
    pub cache: PathBuf,
}

pub fn input() -> Input {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("Cache.Windows");
    fx::textures().without_oodle().write(&cache).unwrap();
    // Models and sounds share the Misc package, so they go in as one fixture.
    let mut misc = fx::models().without_oodle();
    misc.packages[0].files.extend(fx::sounds().without_oodle().packages.remove(0).files);
    misc.write(&cache).unwrap();
    Input { dir, cache }
}

/// How many live files the three fixtures hold between them.
pub const FILES: usize = 13 + 7 + 6;
