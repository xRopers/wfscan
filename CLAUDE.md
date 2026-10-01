# wfscan — Warframe cache indexer and exporter

A sibling of zscan (`E:\zscan`), texscan (`E:\texscan`) and audscan (`E:\audscan`): same structure, conventions and style. zscan is the model for the workspace and the GUI layering; texscan and audscan are the models for format files.

## Goal
A ground-up Rust replacement for Puxtril's Warframe-Exporter (C++/Qt, github.com/Puxtril/Warframe-Exporter) and its cache library LotusLib. Read Warframe's (and Soulframe's) `Cache.Windows` packages, index every file into a manifest, and export models, textures, materials, audio, levels and shaders to standard formats. One CLI, manifest-driven, with a per-file result for everything it touches; a GUI later on the same core library.

Read-only by design: it never writes to the cache. There is no pack command (see "Not doing").

## Clean-room rule
The original repo and LotusLib have **no license file**, so their code cannot be copied or translated. Use them, and the `.hexpat` patterns in the original's `util/` folder, only to learn what the formats are; write every reader from the format description and from real files. Record where each format fact came from in the format file's doc comment.

## Language and key crates
- Rust workspace, edition 2024, MSRV 1.89 (GUI 1.95 for egui 0.36), license GPL-2.0-or-later, like the siblings. Check licenses before adding a crate; ask before adding Apache-2.0-only code.
- `memmap2` (the `.cache` files are tens of GB), `rayon`, `clap`, `serde` + `serde_json`, `crc32fast` or `xxhash-rust`, `thiserror`/`anyhow`
- Siblings are git dependencies (github.com/xRopers/...), pinned to a tag, never path dependencies.
- Oodle: zscan-core's `oodle` plugin (user's own `oo2core_*` loaded with `libloading`; never bundled), behind wfscan's `oodle` cargo feature, which is on by default because almost every block is Oodle. Warframe does not ship an oo2core DLL, so later evaluate a pure-Rust Kraken decoder (decode is all an exporter needs) to remove the setup step. Check its license and test it against oo2core on real blocks before relying on it.
- Textures: wfscan writes DDS itself (texscan-core has a DDS reader, no writer). `texscan-core` (pinned to a commit: it has no tags) decodes BCn and encodes PNG, behind wfscan's `png` cargo feature, on by default. texscan-core includes `basisu` (Apache-2.0 only), so binaries with `png` are effectively GPL-3, as texscan's own are.
- Audio (stage 5): `audscan-core` where the payload is a format it already knows.
- Models: glTF 2.0 written directly with `serde_json` (or `gltf-json`), `glam` for math.

## Layout
```
wfscan/
  crates/
    wfscan-core/        # library
      src/cache/        # toc.rs, block.rs (+ lzf.rs, oodle.rs), package.rs (H/B/F trio, game, category), header.rs (CommonHeader)
      src/index.rs      # cache -> manifest, parallel, fail-soft
      src/manifest.rs   # the JSON record of every file
      src/output.rs     # temp-file-then-rename writes, safe internal paths
      src/types.rs      # the type table: game x package category x type number -> Kind
      src/export.rs     # Exporter trait, registry, parallel fail-soft driver, skip rules
      src/diff.rs       # what changed between two manifests
      src/report.rs     # coverage per type, failures grouped
      src/formats/      # one folder per asset kind; mod.rs has FormatOptions and Exporters::builtin
        texture/        # raw.rs (header), model.rs (Texture: format, size, mips, shape, DDS-order data), dds.rs (writer), mod.rs (exporter, PNG)
                        # (stage 4+) model/, material/, audio/, level/, landscape/, shader/
    wfscan-cli/         # clap CLI, binary `wfscan`
    wfscan-fixtures/    # deterministic synthetic caches, written without wfscan-core; `gen-fixtures` writes tests/fixtures/
    wfscan-gui/         # (stage 8) egui desktop app, depends on wfscan-core only
  tests/fixtures/       # generated <name>/Cache.Windows + expected.json, checked in
```
The core must never depend on the CLI or GUI. The GUI calls the library directly.

Each asset kind keeps the original's three steps, because it is a good design: read into a struct that mirrors the file (`raw`), convert to a game-agnostic struct (`model`), then write a standard format (`write`).

## CLI
```
wfscan info    <Cache.Windows>                             # game, packages, counts, sizes (TOCs only, no Oodle needed)
wfscan index   <Cache.Windows> [-o manifest.json] [--hash] [--full-headers] [--no-stale]
wfscan list    -m manifest.json [--package P] [--path PREFIX] [--type N] [--kind K] [--status S] [--failed] [--stale]
wfscan raw     <Cache.Windows> --path FILE|FOLDER/ [--package P] -d out/   # decompressed H/B/F bytes, for format work
wfscan export  <Cache.Windows> -m manifest.json -d out/ [--package P] [--path PREFIX] [--kinds K,K] [--texture-format dds|png] [--changed-since old.json] [--dry-run] [--force]
wfscan diff    old.json new.json [--list]                  # added / removed / changed, totals per type
wfscan report  -m manifest.json [--errors N] [--unknown]   # coverage per type: files, kind, read and export results; failures grouped
```
Global: `--json`, `-j N`, `--oodle-dll PATH` (default `$WFSCAN_OODLE_DLL`, then zscan's `$ZSCAN_OODLE_DLL`), `--allow-in-use`. Exit code is non-zero only for a fatal error; per-file failures are reported and counted.

## Manifest (per file)
One JSON line per (package, internal path), sorted, stale copies after the live one.
- package, path, `stale`
- `h` / `b` / `f`: offset, compressed and decompressed size, TOC timestamp (FILETIME), and `crc32` of the contents with `--hash`
- `header`: type number and the header's 16-byte hash; source paths and attribute text with `--full-headers`
- `error`: which part couldn't be read and why
- `export` (set by `wfscan export`): status (`ok` | `failed` | `unsupported` | `skipped`), exporter and version (`texture@1`), reason, output files
- The kind of asset is not stored: it is looked up from the type table each time (`Manifest::classifier`), so a table change applies to old manifests too.

## Requirements / improvements over the original
1. **Manifest and incremental export.** Index once, then export only what changed since the last manifest. Warframe updates weekly; re-exporting the whole cache each time is the original's biggest cost.
2. **Update diffs.** `diff` between two manifests shows what a patch added, changed or removed. TOC entries for deleted or superseded files are kept in the index and can be exported (original issue 37).
3. **Fail-soft with a coverage report.** One bad file never stops a run or crashes it. Every file ends with a status; `report` groups failures by type enum so a format broken by a game update shows up as one line, not a crash (original issues 4, 7, 38, 40, 44, 55, 59, 62).
4. **Parallel.** mmap the caches, decompress and convert on a rayon pool (original issue 25, open since the start). Bounded memory: stream large outputs, no whole-level buffers (issue 15, >4 GB overflow).
5. **Types as data.** The type-enum tables (game × package category × enum → reader) live in one table file with a version note per entry, so a new enum is a one-line change and `report` lists unknown enums with counts and sample paths.
6. **Safety.** Read-only, never touches the cache, refuses to run against a cache that is locked by the running game instead of half-reading it. `--dry-run` lists what would be written. Outputs are written to a temp file and renamed.
7. **Verification.** Every reader checks it consumed exactly the bytes it expected; glTF output is validated structurally (accessor bounds, index ranges); textures check mip chain sizes against the header. A mismatch is a `failed` status with the offset, not silent garbage.
8. **Later:** gaps the original still has open — Languages.bin (20), Bink video passthrough (47), newer texture compression formats (43), vertex colours (10), level hierarchy (27), animation scaling (16).

## Build order
1. Workspace skeleton; TOC reader, package trios, block decompression (Oodle + the legacy LZ), CommonHeader; `index`, `list`, `raw`, `info`. Synthetic fixture caches. This replaces LotusLib.
2. Export driver: parallel, fail-soft, statuses in the manifest, `report`, `diff`, `--changed-since`.
3. Textures (DDS, PNG) through texscan-core. Easiest format and the best test of the pipeline.
4. Models to glTF: static, then rigged, then materials and texture references.
5. Audio, then shaders (binary only at first).
6. Levels, static levels and landscapes (they depend on models and materials).
7. Packages.bin and Languages.bin.
8. GUI (egui, zscan's layering: `session.rs` state, `jobs.rs` worker, `app.rs`/`widgets.rs` drawing): package tree with search, type and status columns, texture / model / audio preview, export queue, coverage and diff views.

## Testing
- Fixtures are synthetic caches built by `wfscan-fixtures` (no game data in the repo). Each format gets a minimal valid file, a truncated one and a wrong-enum one.
- Real-cache tests run only when `WFSCAN_CACHE_DIR` is set (like zscan's `ZSCAN_OODLE_DLL`); they assert the coverage numbers do not go down.
- Golden outputs for a handful of exports, compared by hash.

## Not doing
- **Pack / reinject.** zscan's round trip does not carry over: writing to an online game's cache is a terms-of-service problem and the launcher's verify would undo it anyway. The "round trip" here is index → export → report.
- Bundling Oodle or any game data.

## Status
- Stage 3 done: textures (`formats/texture`), the first real exporter. `wfscan export ... --kinds texture [--texture-format dds|png]`.
  - Result on the real cache: all 129,645 files of the `Texture` and `LightMap` packages get a status in a 16.5 s dry run: 128,668 ok, 977 unsupported (pixel formats 31, 32, 33, which the original doesn't know either), 0 failed. As DDS they would be 97.7 GiB. A real folder (321 Excalibur textures): DDS 0.2 s, PNG 3.6 s.
  - Layout, worked out from the cache and written up in `raw.rs` and `model.rs`: the header's first two bytes are the mip counts in B and F; B holds the smallest mips and F the largest (icons keep everything in F); within a part mips run smallest first, and each mip holds all elements (faces, layers, slices). The stored size is the authored size scaled so its larger side equals a third size field: many textures ship at half or quarter size. The element count isn't stored: it is data size / one element's chain. 6 in a cube type (Texture 177, LightMap 177/183) is a cube, several with one mip is a volume, otherwise an array.
  - How it was checked: (1) the size rule fits every texture in a known format; (2) `tests/textures.rs` `real_textures_have_consistent_mips` exports five folders and checks each mip against the box average of the one above (median distance 3.8 of 255; 15 of 4,452 pairs over 30, all small mips of hard-edged graphics); (3) by eye: a store icon, lightmap cube faces, the 5-layer `HeadPoresDetailArray` (the original's issue tracker also says 5).
  - Not checked: the cube face order (written in stored order as +X, -X, +Y, -Y, +Z, -Z), sRGB vs linear (written as UNORM), whether format 0 is really BC1 (sizes fit 8-byte blocks, and the original says so).
  - DDS always has the DX10 header. PNG is the top mip only, one file per face (`_px`...), layer (`_a0`...) or slice (`_z0`...); BC6H is clamped to 0-1 (the original writes Radiance `.hdr` for those; not done); A8 is written as grey.
  - Output names: a known image extension (`.png`, `.tga`, `.hdr`, `.raw`...) is replaced by `.dds`/`.png`, anything else is kept and the extension appended. Two sources that differ only in extension would collide; not handled.
  - The exporter's name includes the format (`texture-dds@1`, `texture-png@1`), so switching format exports again and a manifest holds one set of outputs per file.
  - Fixture textures (`fx::textures()`, `fx::texture(...)`) are solid colours that encode (element, mip), so the tests check every image lands in the right place after reading the DDS back with texscan.
  - LightMap 183 was added to the type table (12,404 `_cross.hdr`, all BC6H cubes).
- Stage 2 done: `wfscan export | diff | report`, the type table, `list --kind/--status`, and the in-use check.
  - Exporters (`export.rs`): implement `Exporter` (`name`, `version`, `kind`, `export(&ExportInput) -> Vec<Output>`) and add it in `Exporters::builtin()` (`formats/mod.rs`). The input gives the parsed common header, the whole H part (`h_body()` is what follows the header) and `read(Part::B | Part::F)`. Outputs are internal-style paths (`/Lotus/A/B.png`) plus bytes; the driver writes them. Return `ExportError::Unsupported` for a variant the reader doesn't handle, `Failed` for a file that should have worked. A panic in an exporter is caught and recorded as `failed`.
  - Not there yet: an exporter can't read *other* files (levels need models, materials need textures), and outputs are whole in-memory buffers (levels will need streaming). Add both to `ExportInput`/`Output` when stage 4-6 needs them.
  - Skip rule: a file is `skipped` when an earlier record is `ok`/`skipped` with the same `name@version` and every output file still exists under `-d`. The earlier record is the manifest's own, else the one in `--changed-since OLD` if `FileEntry::same_content` holds (same header type and hash; per part same sizes and CRC-32, or same timestamp where either side has no CRC; offsets ignored). Failed and unsupported files are always tried again. `--force` exports everything.
  - Whether timestamps survive a game update for untouched files is **not known yet**: nothing has updated since the first index. If they don't, `--changed-since` and `diff` will over-report until manifests are made with `--hash`. Check on the next update: `wfscan diff old.json new.json`.
  - Export refuses a manifest that doesn't match the cache (game, or any package's counts and sizes): "run `wfscan index` again". The manifest is saved only at the end, so an interrupted export records nothing.
  - In-use check (Windows only): opening each cache file while denying writers fails if another process has it open for writing. Tested with a second handle, not with the running game. `info` and manifest-only commands don't check.
  - Type table vs. the real cache (`wfscan report`): 334,737 of 446,777 files are of known kinds. Lines added from the cache: Misc 210 (38,658 materials), ShaderPermutation 32 (12,079 shaders), and in stage 3 LightMap 183. Likely candidates left unmapped until a reader can confirm them: Misc 218 (1,229 `_hlod.hlm`), Misc 212 (35 materials). Big unknowns with no exporter in the original either: Misc 6 (44,336 extensionless files, the samples under `Sounds`), Misc 132 (24,671 `_anim` files; the original has an ImHex pattern for animation type 132), Misc 7/77/38/115 (`.epd`, `.nav`, `.bsp`, `.tmf` files next to levels).
  - Measured: export over all 446,777 files with no exporters 0.03 s (1.5 s with manifest load and save); `report` and `diff` about 1.5 s on two 150-180 MiB manifests.
  - Tests: `tests/export.rs` uses a stand-in model exporter that ends every way an export can; `fx::basic_updated()` is `basic` after a game update.
- Stage 1 done: `wfscan info | index | list | raw`. `cargo test --workspace` (also `--no-default-features`); clippy clean with `-D warnings`; MSRV 1.89 checked.
- Real cache on this machine: `D:\SteamLibrary\steamapps\common\Warframe\Cache.Windows` (September 2026: 15 packages, 446,777 files, 1 stale, 50 GiB on disk, 124 GiB decompressed). An oo2core that works: `D:\SteamLibrary\steamapps\common\Battlefield 6\oo2core_9_win64.dll`.
  - Real-cache tests: set `WFSCAN_CACHE_DIR` and `WFSCAN_OODLE_DLL`, then `cargo test --release -p wfscan-core real_ -- --nocapture` (index, export, texture mips). `WFSCAN_FULL=1` makes the index test decode every part.
  - Measured (Core Ultra 9 285K): `index` 1.6 s (reads all 446,778 H parts, 1.1 GiB); `index --hash` 22.5 s (975,203 parts, 124 GiB, 0 failures); `list` loads the 153 MiB manifest in 0.4 s.
- Format facts confirmed on the real cache (details in each file's doc comment):
  - TOC: 8-byte header (magic `4E C6 67 18`, version 20), 96-byte entries; offset -1 marks a directory; directories are numbered from 1 in order of appearance; timestamp 0 marks a stale entry.
  - Entries: stored when compressed size equals size; otherwise blocks with an 8-byte big-endian header, and header + data sizes add up to the entry's compressed size exactly (the reader requires this).
  - Blocks: 1,266,423 Oodle (first byte `0x8C`), 209,621 stored, **0 LZF**. The LZF decoder follows the published format and is tested against the fixtures' own compressor only; it has never seen real game data.
  - Common header: 444,757 of 446,777 files have one. The rest are ~2,000 extensionless files, almost all in Misc, and 25 `.bin` files.
- Games: Warframe and Soulframe read the same way (Soulframe untested, no cache here). Pre-Ensmallening Warframe is detected (compressed H.Misc entries without block headers) and refused: its layout isn't implemented and there is nothing to test it on. Darkness II / Star Trek are not planned.
- Manifest size drove two choices: one file per line (greppable, diffable), and source paths + attributes only with `--full-headers` (with sources the manifest was 302 MiB; without, 153 MiB).
- `index` without `--hash` reads only H parts, so it can't see a damaged B or F part; `--hash` is the full check.
- Oodle goes through zscan-core's codec, which copies each block in and out. Fast enough (5.5 GiB/s over the whole cache); a direct FFI call would avoid the copies if it ever matters.
- Fixtures stand in for Oodle with "flip" blocks (`0x8C` + inverted bytes) and a matching decoder passed to the reader. CLI tests use `Fixture::without_oodle()` and clear the Oodle environment variables.
- No README yet, nothing committed (the repo is `git init`ed only).

## Open decisions
- Oodle: runtime DLL only, or a pure-Rust Kraken decoder as the default with the DLL as fallback.

## Working notes
- Prefer direct, plain explanations with concrete next steps.
- Write tests alongside each stage.
- cargo is at `%USERPROFILE%\.cargo\bin` and is not on the Git Bash PATH; use PowerShell.
