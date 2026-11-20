# Roadmap: the file types wfscan can't export yet

Counts are from the real cache in November 2026 (447,249 files). A dry run of `wfscan export` over everything gave 268,371 ok, 55 failed and **178,823 unsupported** before phase 1, and 317,686 ok, 55 failed and 129,515 unsupported after it. Until a type gets a reader, `--raw-unsupported` writes its decompressed parts as they are.

Each phase is ordered by how much it covers for the work it takes. Phase 0 unblocks the later ones. Phases 1 and 2 are mostly small. Phases 3 to 6 each need real reverse engineering, done from real files as the clean-room rule in CLAUDE.md requires.

What "checked" means below: a sample file was looked at with `wfscan mcp` (`file`, `read_bytes`) while writing this. Everything else is a guess from the file names and the original exporter's issue tracker, to confirm before building on it.

## Phase 0: groundwork

Nothing here exports a new type. It is what phases 3 to 6 need, and it is cheap to do first.

- **Exporters that read other files.** Materials name textures, models name materials, animations need a skeleton, levels place models. Add a lookup to `ExportInput` (by internal path, any package) so an exporter can read another file's parts. Noted as missing since stage 2.
- **Streamed outputs.** Levels can be larger than memory (original issue 15). Let an `Output` be written in pieces instead of one buffer.
- **Coverage from a dry run.** `wfscan export --dry-run` doesn't save the manifest, so `report` can't show its statuses. Add `--report` to `export`, or let a dry run write to a separate manifest, so every phase can measure what it gained.
- **A survey tool per type.** Each reader so far started with a throwaway program run over every file of the type. Keep one in `tools/` that, for a type number, prints header lengths, part sizes, and the bytes common to all files. That's where each phase below starts.

## Phase 1: data that is already a standard format, or plain text — done (November 2026)

One `passthrough` exporter (`crates/wfscan-core/src/formats/passthrough.rs`) with a rule and a check per kind, so a file that doesn't match fails or stays unsupported instead of going out as junk. Four new kinds in the type table: `video`, `flash`, `script`, `sound-bank`.

| Type (package) | Kind | Files | Output | Result on the real cache |
|---|---|---:|---|---|
| 170 (VideoTexture, VideoTextureStreaming) | video | 238 | F as it is: `.bk2` (or `.bik` for Bink 1), after checking the magic and that the size in its header matches | 238 ok, 808 MiB. Original issue 47 |
| 9 (AnimRetarget), 10 (Misc) | flash | 508 | B as it is, `.swf`, after checking `FWS`/`CWS`/`ZWS` (and the length for `FWS`) | 508 ok, 14 MiB |
| 110, 120 (Font) | script | 5,478 | B as it is, `.luac`, after walking its string table | 5,478 ok, 55 MiB. Luau-like bytecode, not decompiled |
| 6 (Misc) | sound-bank | 44,417 | The attribute text, `.txt` | 43,091 ok. 1,296 are empty 32-byte stubs and 30 carry data in B (animation blend sets, not banks): both stay unsupported, with that reason |

Total: **49,315 files** more export. A full dry run now gives 317,686 ok, 55 failed and **129,515 unsupported** (447,256 files).

Tests: `crates/wfscan-core/tests/passthrough.rs`, on the `passthrough` fixture (a good and a bad file of each kind) and, with `WFSCAN_CACHE_DIR`, every file of the four kinds in the real cache (`real_passthrough_files_all_check_out`, no failures allowed).

Moved out of phase 1 after a closer look; they need a reader of their own:

| Type (package) | Files | What it turned out to be |
|---|---:|---|
| 23 (Misc) | 64 | Reverb impulse responses (`_ir.wav`). The type header has 2 channels, 48 kHz and a frame count, but B is half-floats in an arrangement that doesn't match that count (likely partitioned for convolution), so it isn't a plain waveform |
| 97, 98 (Misc) | 18 | Dialogue. Settings in the attribute text, plus a node table in B followed by text records (`Id=0`, `Content=Start`, `Outgoing={…}`) |
| 4, 39 (Misc) | 318 | Text sanitizer lists. B is a binary trie (UTF-16 characters with flags and links), not word lists |

## Phase 2: gaps in the kinds wfscan already reads — in progress

Same exporters, more variants. Each is a known struct with a new layout.

Done (November 2026):

| What | Files | Result |
|---|---:|---|
| Texture pixel formats 31 and 33 | 977 | 31 is `R8G8B8A8_UINT` (933 lightmap indirection volumes, `_vi.raw`, whose texels count through tile coordinates); 33 is `R10G10B10A2_UNORM` (44 film-look LUTs, 33x33x33). Both are 4 bytes a texel, so the existing volume logic places them. A LUT slice exported as PNG shows the expected red/green ramps. Textures are now 129,700 of 129,701 ok |
| Model type 105, plain form | 1,222 | The level (109) header with 102's B counts, and 102's packed vertices split between B and F (`Level105`). B and F are exactly 24 bytes a vertex and 2 an index in every file. All 1,222 ok; a Corpus hangar's `0_a.fbx` drew as a catwalk with its struts, beside the room's `0_c.fbx` wall panels |
| BC6H textures as `.hdr` | (were clamped) | With `--texture-format png`, BC6H textures go out as Radiance `.hdr` (one per face, layer or slice), decoded to floats with `bcdec_rs`, so values above 1 survive. An editor IBL cube has values up to 2.6, and its +Y face is the sky and -Y the floor. The PNG exporter is now `texture-png@2`, so earlier PNG exports are redone |

Still to do:

| What | Files | Notes |
|---|---:|---|
| Texture pixel format 32 | 1 | One GI volume, 4 bytes a texel. Reads plausibly as both R11G11B10 floats and RGB9E5, so left unsupported rather than guessed |
| Model type 108 (landscape meshes) | 1,434 | Header diverges from every known layout right after the first 0x30 bytes |
| Model type 166 (`.tmesh` terrain) | 1,704 | A format of its own: a 65x65 grid, a size (60x60), LOD ranges and a 15-node quadtree in H; heights and indices in B. Ties in with the landscapes in phase 6 |
| Model type 299 (web and patch meshes) | 207 | Header diverges from every known layout |
| Model failures | 55 | 46 rigged (extra data in B, or indices that don't fit), 9 static (8 more bytes per B vertex) |
| Collision-only models | 2,323 | Nothing to draw today. Could go out as a `.glb` of the collision hulls, as an option; needs PhysX's cooked mesh format |
| Vertex colours (`.vc`, Misc 118) | 3,284 | Original issue 10. Per-vertex data for a model, so it needs phase 0's file lookup to find the model it belongs to |

Moved to phase 3: `_hlod.hlm` (Misc 218, 1,229, and one in 216) are HLOD **materials** (shader sources, then `_hd.png` / `.hlpm` texture names, all in H), not meshes; Misc 212 (35, `BuildDepthMipsMaterial` and the like) look like materials too.

Gained so far: **2,199 files** (976 textures, the rest models). A full dry run: 319,885 ok, 55 failed, 127,316 unsupported (447,256 files).

## Phase 3: materials

| Type (package) | Files | Notes |
|---|---:|---|
| 210 (Misc) | 38,728 | The main material type |
| 218, 216 (Misc) | 1,230 | HLOD materials (`_hlod.hlm`), all in H. Moved here from phase 2 |
| 212 (Misc) | 35 | Material-like (`BuildDepthMipsMaterial`); moved from phase 2 |
| 208, 209, 206 (Misc) | 101 | Rarer material types |

- **Reader:** material parameters and texture slots out of the H body. Checked on `ExcaliburBody`: the attribute text is empty, the source paths list its shaders, and about 1,270 bytes follow the 1,180-byte header. The parameters are there.
- **Output:** a `.json` per material (shader, parameters, texture paths).
- **Then link textures into models:** each `.glb` material gets its base colour, normal and packmap textures, using phase 0's file lookup. Models currently carry material names only.

Covers about **38,800 files**. Linking textures also makes the existing model exports far more useful.

## Phase 4: shaders

| Type (package) | Files | Notes |
|---|---:|---|
| 32 (ShaderPermutationDx11) | 12,105 | Compiled permutations (`0_p.hlsl`, ...). B doesn't start with `DXBC`, so there's a header or table first |
| 42 (ShaderPermutationDx11) | 12 | Compute permutations (`_csp.hlsl`) |
| 24 (ShaderPermutationDx11) | 1 | `VertexInputLayout` |
| 35, 45, 27 (ShaderDx11) | 407 | Shader "sources". Checked: B isn't plain text, so they're compressed or encoded |
| 6 (ShaderDictionaryDx11) | 1 | The dictionary that ties them together |

Plan, as in the build order: binary first. Find the DXBC container inside each permutation and write it as `.dxbc` (tools like `fxc /dumpbin` and RenderDoc read those). Writing out readable HLSL from them isn't planned.

Covers about **12,500 files**.

## Phase 5: animation

| Type (package) | Files | Notes |
|---|---:|---|
| 132 (Misc) | 24,678 | `_anim` clips. The original has an ImHex pattern for type 132 (learn from it; don't copy it). Checked: the H part names the skeleton and lists bone names (`GAME_C1_ROOT`, ...) with parent indices; the keys are in B |
| 134 (Misc) | 1,469 | Cinematic clips (`_cin.fbx`), 1.6 GB in total |
| 11 (AnimRetarget) | 3,455 | Retarget maps between skeletons |
| 19, 14, 16 (Misc) | 706 | Aim poses, movement sets, foot pose data |
| 383, 388 (Misc, AnimRetarget) | 1,325 | Animation state machines and trees (`.animfsm`, `.animtree`) |
| 30 (Misc) | 1 | Motion matching resource |

- **Output:** glTF animations. Each clip goes on the skeleton it names, which phase 0's file lookup fetches from the matching `_skel` model. Write it as a `.glb` with that skeleton, or as an animation-only file.
- **Scaling:** original issue 16 is about animations coming out at the wrong scale, so test against a model whose skeleton is already checked (the inverse bind matrix test in `tests/models.rs`).
- **The rest:** state machines, retarget maps and pose sets are data, not motion. JSON at most, and only once clips work.

Covers about **26,000 clips** plus about 5,500 supporting files.

## Phase 6: levels and landscapes

| Type (package) | Files | Notes |
|---|---:|---|
| 212 (AnimRetarget) | 4,602 | `.level`: the placement of every object. 1.4 GB in total. Despite its name, the AnimRetarget package holds levels, Flash UI and cloth data |
| 30 (AnimRetarget) | 411 | `.static`: static level geometry |
| 42 (Misc) | 52 | `.lndscp` landscapes |
| 284, 3, 234, 286, 288, 279 (AnimRetarget) | about 1,600 | Procedural level definitions (Railjack, Duviri, Dojo) |
| 7, 77, 38, 115, 13, 17 (Misc) | about 20,600 | Files next to every level: `.epd`, `.nav` (navigation mesh), `.bsp`, `.tmf`, `.cvr`, `.lpd` |
| 2, 12, 5 (Misc) | 99 | `.zg`, `.svol` (volumes), `.scatter` (scattered props) |

- **Output:** one glTF scene per level. It places each model by reference rather than copying it, so a level stays small. That needs phase 0's file lookup and streamed outputs, phase 2's terrain meshes, and phase 3's materials to look right.
- **Hierarchy:** original issue 27 asks for the level's own object hierarchy, so keep it.
- **Neighbouring files:** the navigation mesh could go out as a simple mesh. The others are engine data with nothing to view; work out what they are once levels work.

Covers about **5,000 levels** and **22,000 supporting files**.

## Phase 7: Packages.bin and Languages.bin

From the build order. Packages.bin describes game items and their properties; Languages.bin holds every in-game string (original issue 20). Neither type is in the tables above. Find where they are in this cache (`find` for `Packages` and `Languages`), then export them as JSON.

## Probably never (raw parts are enough)

Small types with little to view: fonts and font caches (Font 36, Misc 34, 273, 241), cloth data (48), physics setups (308), the search manifest (4), the localisation cache (25), air-flow volumes (15), one emblem (166), `FontMapper` (35), and the 1,999 files without a common header (`B.Cache.Dx11.bin` and friends, likely the engine's own caches). `--raw-unsupported` already writes them. They show up in `report --unknown` if anyone wants to take one on.

## Totals

| Phase | Files it would cover (approx.) | Cumulative share of today's unsupported |
|---|---:|---:|
| 1. Passthrough and text (done) | 49,315 | 28% |
| 2. Gaps in textures and models (2,199 done) | 12,500 | 36% |
| 3. Materials | 38,800 | 58% |
| 4. Shaders (binary) | 12,500 | 65% |
| 5. Animation | 31,600 | 82% |
| 6. Levels | 27,000 | 97% |
| 7. Packages.bin, Languages.bin | a few | |
| Never | about 3,000 | written raw |

The shares are rough: some types counted here are already mapped to a kind with no exporter, and the counts change with each game update. Run `wfscan report` after each phase to get the real number.
