//! Write every fixture cache to a folder (default `tests/fixtures`), each as
//! `<name>/Cache.Windows/` plus `<name>/expected.json`.

use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let out = std::env::args_os().nth(1).map_or_else(|| PathBuf::from("tests/fixtures"), PathBuf::from);
    for fixture in wfscan_fixtures::all() {
        let dir = out.join(fixture.name);
        fixture.write(&dir.join("Cache.Windows"))?;
        let expected = serde_json::to_string_pretty(&fixture.expected()).expect("expected lists serialise");
        std::fs::write(dir.join("expected.json"), expected + "\n")?;
        println!("{}", dir.display());
    }
    Ok(())
}
