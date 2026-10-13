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
- Audio: Vorbis and Opus sounds are written as Ogg without re-encoding (the writer is wfscan's own). Decoding to PCM, for WAV export and the GUI's player, uses `lewton` (Vorbis, MIT/Apache) and `ropus` (Opus, BSD-3; `opus-decoder` was tried first and was a hundred times slower), behind wfscan's `audio` cargo feature, on by default. The Vorbis header sets the game's sounds need are bundled (`crates/wfscan-core/data/vorbis_headers.bin`): generated with libvorbis 1.3.7 by `tools/vorbis-headers`, under libvorbis's BSD license, not taken from the game. `audscan-core` had nothing these formats need.
- Models: binary glTF 2.0 written directly with `serde_json`; no math crate needed so far.
- GUI: `eframe`/`egui`/`egui_extras` 0.36, `rfd`; `egui_kittest` for tests.

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
        reader.rs       # byte cursor shared by the format readers
        texture/        # raw.rs (header), model.rs (Texture: format, size, mips, shape, DDS-order data), dds.rs (writer), mod.rs (exporter, PNG, decode_rgba)
        model/          # raw.rs (headers and vertex layouts per variant), model.rs (Model: units, meshes, skeleton), glb.rs (writer), render.rs (software preview), mod.rs (exporter)
        audio/          # raw.rs (header), model.rs (Audio: codec, packets), vorbis.rs (the bundled header sets; reading them from a game exe), decode.rs (to PCM), ogg.rs + wav.rs (writers), mod.rs (exporter)
      data/             # vorbis_headers.bin + its README and libvorbis's license
                        # (later) material/, level/, landscape/, shader/
    wfscan-cli/         # clap CLI, binary `wfscan`
    wfscan-fixtures/    # deterministic synthetic caches, written without wfscan-core; `gen-fixtures` writes tests/fixtures/
    wfscan-gui/         # egui desktop app, binary `wfscan-gui` (needs Rust 1.95), depends on wfscan-core only
                        # session.rs (state, slow operations), jobs.rs (one worker job at a time), preview.rs (selected file, own thread), tree.rs (files by folder), player.rs (waveOut playback, from audscan-gui), app.rs + widgets.rs (drawing), settings.rs
  tools/vorbis-headers/ # own workspace: builds libvorbis 1.3.7 (vorbis-sys) and writes data/vorbis_headers.bin; `--check GAME.exe` compares
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
wfscan export  <Cache.Windows> -m manifest.json -d out/ [--package P] [--path PREFIX] [--kinds K,K] [--texture-format dds|png] [--audio-format native|wav] [--game-exe PATH] [--changed-since old.json] [--dry-run] [--force]
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
- GUI additions (October 2026, at the user's request): a tree view, hex/text tabs and a sound player.
  - **List / Tree toggle** in the toolbar. The tree (`tree.rs`) is packages, then folders, folders before files, built from the files the filter lets through; only open folders' lines exist, so all 446,923 files are no slower than the list. Folders start closed; with any filter set they start open (a search is no use folded away). Selecting a file opens the way to it. `⊞` / `⊟` open and close everything. Sorting applies to the list only.
  - **Sort by** dropdown next to the filter (package then path, path, file name, kind, type, size, export status) with an ascending/descending button; the column headings still sort too. Path and name ignore case.
  - **Preview / Hex / Text tabs** in the details pane, as in zscan-gui. Hex and Text show one part of the file as it is in the cache once decompressed (H, B or F, chosen next to the tabs; the first 4 MiB). A kind with no viewer shows its bytes under Preview. The tab chosen stays as you move between files.
  - **Sound player**: waveform, Play/Stop, click the waveform to play from there, Space plays and stops; playback stops when another file is selected. Playback is Windows-only (waveOut, `player.rs` copied from audscan-gui). Not heard by ear from here: the device test is `cargo test -p wfscan-gui -- --ignored`.
  - Checked by eye on the real cache (tree, a Vorbis and an Opus sound, hex tab, filtered tree) with the temporary kittest screenshots described below.
- Stage 5, audio half, done: sounds (`formats/audio`) export with `wfscan export ... --kinds audio [--audio-format native|wav] [--game-exe PATH]`, and from the GUI (Export menu, "Sounds as").
  - Result on the real cache (dry runs): all 88,780 sounds ok in both formats, 0 failed, 0 unsupported. Native: 9.5 GiB in 4.7 s. WAV: 84 GiB.
  - Native writes each sound in its own format with nothing re-encoded: Vorbis as `.ogg` (the bundled header packets, then the sound's packets, page positions from the game's packet table), Opus as `.opus` (an `OpusHead`/`OpusTags` written here, pre-skip 312 or what the packets have to spare), PCM as `.wav`. WAV decodes everything to 16-bit PCM.
  - Checked (`tests/audio.rs` `real_sounds_export_and_read_back`): 32 exported `.ogg` files read with lewton's own Ogg reader (which checks page checksums) and 136 `.opus` files (48 and 24 kHz) read page by page decode to exactly the samples the sound decodes to directly, and end at the sound's length. **Not checked with an outside player** (no ffmpeg or similar on this machine): play a few exported files in VLC or foobar2000 before trusting a full export.
  - **No game executable is needed.** The game keeps the Vorbis header packets in `Warframe.x64.exe`, but all 12 sets there are byte for byte what libvorbis 1.3.7 writes for `vorbis_encode_init_vbr` at qualities -0.1 and 0.45 (32 kHz) and 0.725, 0.78, 0.835, 0.89 (44.1 kHz), mono and stereo. wfscan bundles the libvorbis-generated sets (see `data/README.md`). `--game-exe PATH` adds whatever sets an executable has that aren't bundled, for after a game update; `real_sounds_decode` fails if the game next to the cache has a set that isn't bundled.
  - An `.ogg` exported with the wrong one of the two 44.1 kHz setups (see the guess below) is a valid file full of noise. The exporter picks the set by the same trial decode as the player; without the `audio` feature it takes the later set.
  - Still owed by stage 5: shaders.
  - Result on the real cache with `WFSCAN_FULL=1 cargo test --release -p wfscan-core --test audio real_sounds_decode` (190 s): all 88,780 sounds of type 139 (Misc and its language packages) decode to exactly the sample count their header states: 46,723 Vorbis, 41,982 Opus, 75 PCM, 0 failures. 107 clip on more than 2% of their samples (99 Vorbis, 8 Opus): loud sounds, or for Vorbis possibly the wrong header set.
  - **Codec 8 is Vorbis**, which the original doesn't read at all (it knows 0 PCM, 5 ADPCM, 7 Opus): raw packets with no container and no header packets, behind a table of packet offsets and sample positions in B. ADPCM doesn't occur in the cache and has no reader.
  - The Vorbis header packets aren't in the cache (searched: no setup header anywhere in 124 GiB) but in the executable; `VorbisTables::scan` reads them from one, `VorbisTables::bundled` is the built-in copy.
  - **Which set a 44.1 kHz sound uses is a guess.** There are two setups per channel count and no header field found that picks one (most sounds use the second, a few hundred the first). `decode` tries both on the first 400 packets and keeps the one that clips least, then the quieter. On quiet sounds that can pick wrong, and the result is noise. The exporter uses the same trial; a real rule would be better.
  - Data rule (from the original, confirmed): the audio is the last `size` bytes of B followed by F; anything before it in B is other data. Opus: fixed-size packets of 20 ms, and 312 samples (at 48 kHz) or fewer of lead-in to drop (51 of 229 sampled sounds had only 256 to spare). Some dialogue is 24 kHz Opus: the header's counts are at that rate, Ogg Opus positions always at 48 kHz.
  - Fixtures: `fx::sounds()` (PCM stored three ways, a Vorbis sound of noise packets that reads but can't decode, ADPCM, a truncated one). There is no fixture Vorbis or Opus that decodes: only the real-cache test covers the decoders.
- GUI (`crates/wfscan-gui`, egui/eframe 0.36, file dialogs via `rfd`), built early at the user's request, ahead of stages 5-7: `cargo run -p wfscan-gui --release -- [--oodle-dll PATH] [Cache.Windows]`.
  - What it does: open a cache folder (indexes it, 1.6 s on the real cache); a virtualized table of every file (package, kind, type, size, export status, path), sortable, filtered by path text, kind, status and "known kinds only"; a details pane with the file's parts, read and export results, its attribute text and a preview: textures decoded (largest mip up to 1024), models drawn by the core's software renderer (drag to turn, scroll to zoom), anything else as hex (see the additions above); export of the selected file or of everything shown, textures as DDS or PNG; Tools > Oodle library (remembered in `wfscan/gui.json`, reopens a cache that was opened without it), Coverage (the report) and Log.
  - Layers as in zscan-gui: `session.rs` has no drawing code; `jobs.rs` runs one job at a time and applies its result on the UI thread; `preview.rs` loads the selected file on its own thread and drops stale results. The table's row list is recomputed only when the filter, sort or session generation changes (it filters 446,777 files).
  - An export clones the manifest into the job and swaps the result in when it finishes.
  - Tests: `tests/workflow.rs` drives the session through jobs; `tests/ui.rs` clicks through the real window with `egui_kittest` on a cache made of the texture and model fixtures. To look at the window, temporarily enable kittest's `wgpu` + `snapshot` features and save `harness.render()` to PNG (done once on the real cache: table, model and texture previews and the coverage window all drew correctly). Don't commit that.
  - Not done: progress bars and cancelling (jobs only show a spinner and elapsed time), saving or loading a manifest, `diff` between manifests, choosing which cube face, layer or slice to show, textures on models, exporting a whole folder from the tree. The GUI's own MSRV (1.95) is only checked in CI: that toolchain isn't installed here.
  - With the GUI in the workspace, `--no-default-features` no longer turns the core's features off (the GUI always enables them), so the featureless build and tests add `--exclude wfscan-gui`.
- Stage 4 done, in part: models (`formats/model`) as binary glTF, `wfscan export ... --kinds model`.
  - Read: Warframe types 102 (static), 109 (level) and 281 (rigged), and the `_dcm` / `_hlod` forms of 102 and 105 (none of those two forms occur in the current cache, so they are untested on real data). Result on the real cache, 57,698 model files in 12.8 s, 4.9 GiB of `.glb`: 50,753 ok, 55 failed, 6,890 unsupported (2,323 collision-only files with nothing to draw; 4,567 of types with no reader: 166 terrain 1,704, 108 1,434, plain 105 1,222, 299 207). Soulframe models: no reader.
  - The 55 failures are layout variants not understood yet: 46 rigged (extra data in B, or indices that don't fit) and 9 static (8 more bytes per B vertex).
  - Added to what the original describes, from the cache: static models with a morph count carry `morph count x 8` bytes after their indices (595 models); B counts of zero mean everything is in B (also for static models); weighted bone names missing from the bone tree (213 models, mostly an empty name) get a bone of their own at the root so joint numbers still line up.
  - Measured: bone rotations are stored x, y, z, w. The skeleton is mirrored in X (position x negated, rotation y and z negated, first row of each inverse bind matrix negated) and the vertices are left alone, so skinning mirrors them; static models have Z negated instead. Both follow the original. Check (`tests/models.rs`): every one of 2,346 real joints' world transform times its inverse bind matrix is that mirror, median error 0.
  - **Normals and tangents are not exported.** The four normal bytes don't decode to unit vectors or line up with the triangles under any reading tried (bytes as -1..1, 10-bit fields, a tangent-frame quaternion); the original doesn't decode them either. Viewers compute their own. Winding is unverified for the same reason; materials are double-sided.
  - Materials are names only: each mesh's material path is in the glTF material's `extras.path` (from the header for level models, else from the `Materials={...}` attribute text; relative names are next to the model). No textures are linked: that needs the material reader and exporters that can read other files. Only LOD 0 of each mesh is exported. Ambient occlusion goes out as `_OCCLUSION`.
  - `formats/model/render.rs` is a small software renderer (flat shading, z-buffer), used by the GUI's preview and to check models by eye: Excalibur's body and helmet drew upright and in proportion.
  - `export::with_input` reads one file the way the export driver would, without a manifest: used by previews and the real-cache tests.
  - Fixtures: `fx::models()` (a quad as a static, a level and a rigged model, plus collision-only, unreadable types and a truncated one); model readers were first checked against all 57,698 real models with a throwaway survey tool.
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
- Published at github.com/xRopers/wfscan (public, like the siblings). Release v0.1.0 (October 2026): a Windows x64 zip of both programs built with the C runtime linked in (`RUSTFLAGS="-C target-feature=+crt-static" cargo build --release --target x86_64-pc-windows-msvc -p wfscan-cli -p wfscan-gui`; without `--target` that build fails in a build script), the README with its screenshots, LICENSE and `licenses/COPYING-libvorbis`. The README screenshots show file names and a waveform only: no game art.
- The game updated between the first index and October 2026 (446,777 files with 1 stale then; 446,923 with 1,269 stale now). No manifest from before that update was kept, so the timestamp question in the stage 2 notes is still open: save a manifest now (`wfscan index ... -o`) and run `wfscan diff` against a new one after the next update.

## Open decisions
- Oodle: runtime DLL only, or a pure-Rust Kraken decoder as the default with the DLL as fallback.

## Working notes
- Prefer direct, plain explanations with concrete next steps.
- Write tests alongside each stage.
- cargo is at `%USERPROFILE%\.cargo\bin` and is not on the Git Bash PATH; use PowerShell.
