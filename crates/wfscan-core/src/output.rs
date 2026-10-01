//! Writing output files: to a temporary name first, renamed into place once complete, so
//! a failed or interrupted run never leaves a half-written file under the real name.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use crate::error::{Error, Result, io_err};

/// Create `path` (and its parent folders) with what `write` produces.
pub fn write_atomic(path: &Path, write: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let result = (|| {
        let mut w = BufWriter::new(File::create(&tmp)?);
        write(&mut w)?;
        w.flush()?;
        drop(w);
        std::fs::rename(&tmp, path)
    })();
    result.map_err(|source| {
        std::fs::remove_file(&tmp).ok();
        Error::Io { path: path.to_path_buf(), source }
    })
}

/// The file an internal path such as `/Lotus/A/b.png` maps to under `dir`. Internal paths
/// come from the cache, so anything that could step outside `dir` is refused.
pub fn internal_path_under(dir: &Path, internal: &str) -> Result<std::path::PathBuf> {
    let mut out = dir.to_path_buf();
    let mut any = false;
    for part in internal.split('/').filter(|p| !p.is_empty()) {
        if part == "." || part == ".." || part.contains(['\\', ':']) {
            return Err(Error::BadPath(internal.to_string()));
        }
        out.push(part);
        any = true;
    }
    if !any {
        return Err(Error::BadPath(internal.to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_paths_stay_inside() {
        let dir = Path::new("out");
        assert_eq!(internal_path_under(dir, "/Lotus/A/b.png").unwrap(), dir.join("Lotus").join("A").join("b.png"));
        for bad in ["/Lotus/../../x", "/C:/x", "/a\\b", "/", "", "/./x"] {
            assert!(matches!(internal_path_under(dir, bad), Err(Error::BadPath(_))), "{bad}");
        }
    }

    #[test]
    fn writes_then_renames() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a").join("b.txt");
        write_atomic(&path, |w| w.write_all(b"hello")).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        let failed = write_atomic(&path, |_| Err(io::Error::other("boom")));
        assert!(failed.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"hello", "a failed write leaves the old file");
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "and no temp file");
    }
}
