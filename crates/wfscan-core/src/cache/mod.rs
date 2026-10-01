//! The cache on disk: `<part>.<package>.toc` + `.cache` pairs, one file per format fact.

pub mod block;
pub mod header;
pub mod lzf;
pub mod oodle;
pub mod package;
pub mod toc;
