//! Model export, checked by reading the `.glb` files back: on the synthetic `models`
//! fixture, where the geometry is known, and on a real cache, where each skeleton must
//! agree with its own bind matrices.

use std::path::{Path, PathBuf};

use serde_json::Value;
use wfscan_core::formats::model::{self, Model, glb};
use wfscan_core::{
    Cache, ExportOptions, ExportRecord, ExportStats, Exporters, FormatOptions, IndexOptions, Kind, Manifest, OodleDecoder, Status, export,
    index, with_input,
};
use wfscan_fixtures::{self as fx, mdl};

struct Setup {
    tmp: tempfile::TempDir,
    cache: Cache,
    manifest: Manifest,
    stats: ExportStats,
}

impl Setup {
    fn glb(&self, internal: &str) -> Vec<u8> {
        let path = internal.split('/').filter(|p| !p.is_empty()).fold(self.tmp.path().join("out"), |dir, part| dir.join(part));
        std::fs::read(path).unwrap()
    }

    fn record(&self, path: &str) -> &ExportRecord {
        self.manifest.files.iter().find(|f| f.path == path).unwrap().export.as_ref().unwrap()
    }
}

fn exported() -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Cache.Windows");
    fx::models().write(&dir).unwrap();
    let cache = Cache::open(&dir).unwrap();
    let (mut manifest, index_stats) = index(&cache, None, &IndexOptions::default()).unwrap();
    assert_eq!(index_stats.failed, 0);
    let exporters = Exporters::builtin(&FormatOptions::default());
    let stats = export(&cache, &mut manifest, None, &exporters, None, &tmp.path().join("out"), &ExportOptions::default()).unwrap();
    Setup { tmp, cache, manifest, stats }
}

/// An accessor's values as f32s (whatever it stores them as) and its component count.
fn accessor(document: &Value, bin: &[u8], index: &Value) -> (Vec<f32>, usize) {
    let accessor = &document["accessors"][index.as_u64().expect("an accessor index") as usize];
    let view = &document["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
    let bytes = &bin[view["byteOffset"].as_u64().unwrap() as usize..][..view["byteLength"].as_u64().unwrap() as usize];
    let components = match accessor["type"].as_str().unwrap() {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        "MAT4" => 16,
        other => panic!("accessor type {other}"),
    };
    let values: Vec<f32> = match accessor["componentType"].as_u64().unwrap() {
        5121 => bytes.iter().map(|&b| f32::from(b)).collect(),
        5123 => bytes.as_chunks::<2>().0.iter().map(|b| f32::from(u16::from_le_bytes(*b))).collect(),
        5125 => bytes.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b) as f32).collect(),
        5126 => bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect(),
        other => panic!("component type {other}"),
    };
    assert_eq!(values.len(), accessor["count"].as_u64().unwrap() as usize * components);
    (values, components)
}

/// The quad's positions in units, with Z negated if `mirrored`.
fn expected_positions(mirrored: bool) -> Vec<f32> {
    let z = if mirrored { -1.0 } else { 1.0 };
    fx::MODEL_POSITIONS.iter().flat_map(|p| [p[0] * fx::MODEL_SCALE[0], p[1] * fx::MODEL_SCALE[1], p[2] * fx::MODEL_SCALE[2] * z]).collect()
}

fn assert_close(got: &[f32], want: &[f32], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}");
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() < 2e-4, "{what}: {got:?} is not {want:?}");
    }
}

/// The checks every exported fixture model passes: two meshes of one triangle each over
/// the quad's four vertices.
fn check_quad(document: &Value, bin: &[u8], mirrored: bool, materials: [&str; 2], what: &str) {
    let meshes = document["meshes"].as_array().unwrap();
    assert_eq!(meshes.iter().map(|m| m["name"].as_str().unwrap()).collect::<Vec<_>>(), fx::MODEL_MESHES, "{what}");
    for (i, mesh) in meshes.iter().enumerate() {
        let primitive = &mesh["primitives"][0];
        let (positions, _) = accessor(document, bin, &primitive["attributes"]["POSITION"]);
        assert_close(&positions, &expected_positions(mirrored), what);
        let (uvs, _) = accessor(document, bin, &primitive["attributes"]["TEXCOORD_0"]);
        assert_close(&uvs, &fx::MODEL_UVS.concat(), what);
        let (occlusion, _) = accessor(document, bin, &primitive["attributes"]["_OCCLUSION"]);
        assert_eq!(occlusion, fx::MODEL_OCCLUSION.map(f32::from), "{what}");
        let (indices, _) = accessor(document, bin, &primitive["indices"]);
        assert_eq!(indices, fx::MODEL_INDICES[i * 3..i * 3 + 3].iter().map(|&v| f32::from(v)).collect::<Vec<_>>(), "{what}");
        let material = &document["materials"][primitive["material"].as_u64().unwrap() as usize];
        assert_eq!(material["extras"]["path"], materials[i], "{what}");
        assert_eq!(mesh["extras"]["scale"].as_array().unwrap().len(), 3);
    }
}

#[test]
fn static_and_level_models() {
    let s = exported();
    assert_eq!((s.stats.selected, s.stats.ok, s.stats.unsupported, s.stats.failed), (7, 3, 3, 1));

    let record = s.record(mdl::STATIC);
    assert_eq!((record.status, record.exporter.as_deref(), &record.outputs[..]), (Status::Ok, Some("model-glb@1"), &["/Lotus/Objects/Crate/Crate.glb".to_string()][..]));
    let file = s.glb("/Lotus/Objects/Crate/Crate.glb");
    let (document, bin) = glb::parse(&file).expect("a well-formed glb");
    check_quad(&document, bin, true, ["/Lotus/Materials/FrontMat", "/Lotus/Objects/Crate/BackMat"], "static");
    assert!(document.get("skins").is_none());
    assert_eq!(document["meshes"][0]["extras"]["source"], mdl::STATIC);
    assert_eq!(document["materials"][1]["name"], "BackMat");

    // The level model has its positions in units already and lists its own materials.
    let file = s.glb("/Lotus/Levels/Room/0_c.glb");
    let (document, bin) = glb::parse(&file).unwrap();
    check_quad(&document, bin, true, ["/Lotus/Levels/Room/WallMat", "/Lotus/Levels/Room/FloorMat"], "level");
}

#[test]
fn a_rigged_model_keeps_its_skeleton() {
    let s = exported();
    let file = s.glb("/Lotus/Characters/Hero/Hero_skel.glb");
    let (document, bin) = glb::parse(&file).unwrap();
    check_quad(&document, bin, false, ["/Lotus/Materials/FrontMat", "/Lotus/Characters/Hero/BackMat"], "rigged");

    let nodes = document["nodes"].as_array().unwrap();
    let bones: Vec<&Value> = nodes.iter().filter(|n| n.get("mesh").is_none()).collect();
    assert_eq!(bones.iter().map(|b| b["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Root", "Spine", "Arm"]);
    // Root > Spine > Arm, with the mesh nodes first.
    assert_eq!((&nodes[2]["children"], &nodes[3]["children"], nodes[4].get("children")), (&serde_json::json!([3]), &serde_json::json!([4]), None));
    let skin = &document["skins"][0];
    assert_eq!(skin["joints"], serde_json::json!([4, 2, 3]), "Arm and Root are weighted, Spine follows");
    assert_eq!((&nodes[0]["skin"], &nodes[1]["skin"]), (&serde_json::json!(0), &serde_json::json!(0)));

    let attributes = &document["meshes"][0]["primitives"][0]["attributes"];
    let (joints, _) = accessor(&document, bin, &attributes["JOINTS_0"]);
    assert_eq!(joints, [1.0, 0.0, 0.0, 0.0].repeat(4), "local bones 0 and 1 are joints 1 (Root) and 0 (Arm)");
    let (weights, _) = accessor(&document, bin, &attributes["WEIGHTS_0"]);
    assert_close(&weights[..8], &[1.0, 0.0, 0.0, 0.0, 512.0 / 1023.0, 511.0 / 1023.0, 0.0, 0.0], "weights");

    // Mirrored in X: the Spine's quarter turn about Y runs the other way, and the Arm's
    // offset changes side.
    let (rotation, translation) = fx::model_bone_transform(1);
    assert_close(&floats(&nodes[3]["rotation"]), &[rotation[0], -rotation[1], -rotation[2], rotation[3]], "Spine rotation");
    assert_close(&floats(&nodes[3]["translation"]), &translation, "Spine translation");
    assert_close(&floats(&nodes[4]["translation"]), &[-0.5, 0.0, 0.25], "Arm translation");
    let (matrices, _) = accessor(&document, bin, &skin["inverseBindMatrices"]);
    let rows = fx::model_inverse_bind(0);
    let arm: Vec<f32> = (0..4).flat_map(|column| [-rows[column], rows[4 + column], rows[8 + column], if column == 3 { 1.0 } else { 0.0 }]).collect();
    assert_close(&matrices[..16], &arm, "Arm inverse bind, column-major with the first row negated");
}

fn floats(value: &Value) -> Vec<f32> {
    value.as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect()
}

#[test]
fn what_does_not_export_says_why() {
    let s = exported();
    let reason = |path: &str| {
        let record = s.record(path);
        (record.status, record.reason.clone().unwrap())
    };
    assert_eq!(reason(mdl::COLLISION), (Status::Unsupported, "the model has no meshes (collision data only)".into()));
    assert_eq!(reason(mdl::OTHER_TYPE), (Status::Unsupported, "no reader for model type 299 with this name yet".into()));
    assert_eq!(reason(mdl::OTHER_NAME).0, Status::Unsupported);
    let (status, why) = reason(mdl::SHORT);
    assert!(status == Status::Failed && why.starts_with("F: the data ends at"), "{why}");
}

#[test]
fn a_single_file_can_be_read_and_drawn() {
    let s = exported();
    let model = with_input(&s.cache, "Misc", mdl::RIGGED, None, model::read).unwrap().unwrap();
    assert_eq!((model.positions.len(), model.triangle_count(), model.skeleton.as_ref().map(|k| k.bones.len())), (4, 2, Some(3)));
    let picture = model::render::render(&model, 48, 48, &model::render::View::default());
    assert!(picture.as_chunks::<4>().0.iter().filter(|p| p[3] == 255).count() > 100);

    assert!(with_input(&s.cache, "Misc", "/Lotus/Nope.fbx", None, |_| ()).is_err());
    assert!(with_input(&s.cache, "Nope", mdl::RIGGED, None, |_| ()).is_err());
}

type Matrix = [[f32; 4]; 4];

fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [[0.0; 4]; 4];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, value) in row.iter_mut().enumerate() {
            *value = (0..4).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    out
}

/// A bone's local transform as a row-major matrix.
fn local(rotation: [f32; 4], translation: [f32; 3]) -> Matrix {
    let [x, y, z, w] = rotation;
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w), translation[0]],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w), translation[1]],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y), translation[2]],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// How far each weighted joint's world transform times its inverse bind matrix is from a
/// mirror in X, which is what skinning should do to a rigged model at rest.
fn bind_errors(model: &Model) -> Vec<f32> {
    let skeleton = model.skeleton.as_ref().expect("a rigged model");
    let mut world: Vec<Option<Matrix>> = vec![None; skeleton.bones.len()];
    fn resolve(bone: usize, bones: &[model::Bone], world: &mut Vec<Option<Matrix>>) -> Matrix {
        if let Some(done) = world[bone] {
            return done;
        }
        let own = local(bones[bone].rotation, bones[bone].translation);
        let matrix = match bones[bone].parent {
            Some(parent) => multiply(&resolve(parent, bones, world), &own),
            None => own,
        };
        world[bone] = Some(matrix);
        matrix
    }
    skeleton
        .joints
        .iter()
        .zip(&skeleton.inverse_binds)
        .filter(|(_, inverse)| **inverse != [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0])
        .map(|(&bone, inverse)| {
            let mut bind = [[0.0; 4]; 4];
            for (r, row) in bind.iter_mut().enumerate() {
                for (c, value) in row.iter_mut().enumerate() {
                    *value = inverse[c * 4 + r];
                }
            }
            let product = multiply(&resolve(bone, &skeleton.bones, &mut world), &bind);
            let mut squared = 0.0;
            for (r, row) in product.iter().enumerate() {
                for (c, value) in row.iter().enumerate() {
                    let mirror = if r != c { 0.0 } else if r == 0 { -1.0 } else { 1.0 };
                    squared += (value - mirror).powi(2);
                }
            }
            squared.sqrt()
        })
        .collect()
}

#[test]
fn the_fixture_skeleton_agrees_with_its_bind_matrices() {
    let s = exported();
    let model = with_input(&s.cache, "Misc", mdl::RIGGED, None, model::read).unwrap().unwrap();
    let errors = bind_errors(&model);
    assert_eq!(errors.len(), 2, "the weighted bones, Arm and Root");
    assert!(errors.iter().all(|&e| e < 1e-5), "{errors:?}");
}

fn glb_files(dir: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            glb_files(&path, into);
        } else if path.extension().is_some_and(|e| e == "glb") {
            into.push(path);
        }
    }
}

/// Real models: export a few folders, check every `.glb` is well formed, and check that
/// rigged models' skeletons agree with their bind matrices. Needs `WFSCAN_CACHE_DIR` and
/// `WFSCAN_OODLE_DLL`; skipped otherwise. With `WFSCAN_RENDER_DIR` set (and the `png`
/// feature), also writes pictures of a few models there to look at.
#[test]
fn real_models_are_well_formed() {
    let Some(dir) = std::env::var_os("WFSCAN_CACHE_DIR").filter(|d| !d.is_empty()) else {
        eprintln!("skipped: WFSCAN_CACHE_DIR is not set");
        return;
    };
    let Ok(oodle) = wfscan_core::Oodle::load(None) else {
        eprintln!("skipped: no Oodle library");
        return;
    };
    let oodle: &dyn OodleDecoder = &oodle;
    let cache = Cache::open(Path::new(&dir)).unwrap();
    let (mut manifest, _) = index(&cache, Some(oodle), &IndexOptions::default()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let exporters = Exporters::builtin(&FormatOptions::default());
    let folders = ["/Lotus/Characters/Tenno/Excalibur/", "/Lotus/Weapons/Tenno/Melee/Swords/", "/Lotus/Levels/CorpusShipRemastered/ConLjunction02/"];
    for folder in folders {
        let opts = ExportOptions { path: Some(folder.into()), kinds: vec![Kind::Model], ..Default::default() };
        let stats = export(&cache, &mut manifest, None, &exporters, Some(oodle), tmp.path(), &opts).unwrap();
        eprintln!("{folder}: {} ok, {} unsupported, {} failed", stats.ok, stats.unsupported, stats.failed);
        assert!(stats.ok > 0 && stats.failed * 20 <= stats.ok, "{folder}: {stats:?}");
    }

    let mut files = Vec::new();
    glb_files(tmp.path(), &mut files);
    let (mut triangles, mut skinned) = (0usize, 0usize);
    for file in &files {
        let bytes = std::fs::read(file).unwrap();
        let (document, bin) = glb::parse(&bytes).unwrap_or_else(|| panic!("{} is not a glb", file.display()));
        skinned += usize::from(document.get("skins").is_some());
        for mesh in document["meshes"].as_array().unwrap() {
            let primitive = &mesh["primitives"][0];
            let (positions, _) = accessor(&document, bin, &primitive["attributes"]["POSITION"]);
            let (indices, _) = accessor(&document, bin, &primitive["indices"]);
            assert!(positions.iter().all(|p| p.is_finite()), "{}", file.display());
            assert!(indices.iter().all(|&i| (i as usize) < positions.len() / 3), "{}", file.display());
            triangles += indices.len() / 3;
        }
    }
    eprintln!("{} files, {skinned} skinned, {triangles} triangles", files.len());
    assert!(files.len() > 100 && skinned > 20 && triangles > 100_000);

    // Skeletons, straight from the cache.
    let rigged: Vec<&str> = manifest.files.iter().filter(|f| f.path.starts_with(folders[0]) && f.path.ends_with("_skel.fbx")).map(|f| f.path.as_str()).collect();
    let mut errors = Vec::new();
    for path in &rigged {
        if let Ok(Ok(model)) = with_input(&cache, "Misc", path, Some(oodle), model::read)
            && model.skeleton.is_some()
        {
            errors.extend(bind_errors(&model));
        }
    }
    errors.sort_by(f32::total_cmp);
    let median = errors[errors.len() / 2];
    let close = errors.iter().filter(|&&e| e < 0.05).count();
    eprintln!("{} joints of {} rigged models: median bind error {median:.5}, {close} under 0.05", errors.len(), rigged.len());
    assert!(errors.len() > 500 && median < 0.01 && close * 10 > errors.len() * 9, "skeletons and bind matrices disagree");

    #[cfg(feature = "png")]
    if let Some(out) = std::env::var_os("WFSCAN_RENDER_DIR") {
        let pictures = [
            ("/Lotus/Characters/Tenno/Excalibur/ExcaliburBody_skel.fbx", "excalibur"),
            ("/Lotus/Characters/Tenno/Excalibur/ExcaliburHelmet_skel.fbx", "helmet"),
            ("/Lotus/Weapons/Tenno/Melee/Swords/KatanaAndSaya/KatanaAndSaya_skel.fbx", "katana"),
        ];
        for (path, name) in pictures {
            let Ok(Ok(model)) = with_input(&cache, "Misc", path, Some(oodle), model::read) else {
                eprintln!("no picture of {path}");
                continue;
            };
            for (view_name, yaw) in [("front", 0.0f32), ("side", 1.2)] {
                let view = model::render::View { yaw, pitch: 0.1, zoom: 1.0 };
                let rgba = model::render::render(&model, 512, 512, &view);
                let png = texscan_core::encode_png(&texscan_core::Image { width: 512, height: 512, rgba });
                std::fs::write(Path::new(&out).join(format!("{name}_{view_name}.png")), png).unwrap();
            }
        }
    }
}
