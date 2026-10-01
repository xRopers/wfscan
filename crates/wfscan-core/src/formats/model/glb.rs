//! Writing a [`Model`] as binary glTF 2.0 (`.glb`): one file holding the JSON scene
//! description and the vertex data.
//!
//! - One glTF mesh and node per mesh of the model, all sharing the vertex accessors.
//! - Rigged models get their bones as a node tree and one skin.
//! - A material per distinct material path, by name only: `extras.path` holds the
//!   internal path, for whoever pairs it with exported textures.
//! - Ambient occlusion goes out as the custom attribute `_OCCLUSION`.
//!
//! Source: the Khronos glTF 2.0 specification.

use serde_json::{Value, json};

use super::model::Model;

const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const UNSIGNED_BYTE: u32 = 5121;
const UNSIGNED_SHORT: u32 = 5123;
const UNSIGNED_INT: u32 = 5125;
const FLOAT: u32 = 5126;

/// The binary chunk and the views and accessors into it.
#[derive(Default)]
struct Buffers {
    bin: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Buffers {
    /// Add one accessor over `bytes`; returns its index.
    fn add(&mut self, bytes: &[u8], count: usize, component: u32, kind: &str, target: Option<u32>, extra: Value) -> usize {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let mut view = json!({ "buffer": 0, "byteOffset": self.bin.len(), "byteLength": bytes.len() });
        if let Some(target) = target {
            view["target"] = json!(target);
        }
        self.bin.extend_from_slice(bytes);
        self.views.push(view);
        let mut accessor = json!({ "bufferView": self.views.len() - 1, "componentType": component, "count": count, "type": kind });
        if let (Some(accessor), Some(extra)) = (accessor.as_object_mut(), extra.as_object()) {
            accessor.extend(extra.clone());
        }
        self.accessors.push(accessor);
        self.accessors.len() - 1
    }

    fn floats<const N: usize>(&mut self, values: &[[f32; N]], kind: &str, target: Option<u32>, extra: Value) -> usize {
        let bytes: Vec<u8> = values.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
        self.add(&bytes, values.len(), FLOAT, kind, target, extra)
    }
}

fn bounds(positions: &[[f32; 3]]) -> Value {
    let fold = |pick: fn(f32, f32) -> f32, start: f32| [0, 1, 2].map(|axis| positions.iter().fold(start, |acc, p| pick(acc, p[axis])));
    json!({ "min": fold(f32::min, f32::INFINITY), "max": fold(f32::max, f32::NEG_INFINITY) })
}

/// The whole `.glb` file for `model`. `source` is the model's internal path, recorded in
/// the file.
pub fn write(model: &Model, source: &str) -> Vec<u8> {
    let mut buffers = Buffers::default();
    let vertices = model.positions.len();
    let mut attributes = serde_json::Map::new();
    attributes.insert("POSITION".into(), json!(buffers.floats(&model.positions, "VEC3", Some(ARRAY_BUFFER), bounds(&model.positions))));
    attributes.insert("TEXCOORD_0".into(), json!(buffers.floats(&model.uv0, "VEC2", Some(ARRAY_BUFFER), json!({}))));
    attributes.insert("TEXCOORD_1".into(), json!(buffers.floats(&model.uv1, "VEC2", Some(ARRAY_BUFFER), json!({}))));
    let occlusion = buffers.add(&model.occlusion, vertices, UNSIGNED_BYTE, "SCALAR", Some(ARRAY_BUFFER), json!({ "normalized": true }));
    attributes.insert("_OCCLUSION".into(), json!(occlusion));
    if model.skeleton.is_some() {
        let joints: Vec<u8> = model.joints.iter().flatten().flat_map(|j| j.to_le_bytes()).collect();
        attributes.insert("JOINTS_0".into(), json!(buffers.add(&joints, vertices, UNSIGNED_SHORT, "VEC4", Some(ARRAY_BUFFER), json!({}))));
        attributes.insert("WEIGHTS_0".into(), json!(buffers.floats(&model.weights, "VEC4", Some(ARRAY_BUFFER), json!({}))));
    }

    // Materials, one per distinct path, in order of first use.
    let mut material_paths: Vec<&str> = Vec::new();
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    for mesh in &model.meshes {
        let indices = if vertices <= usize::from(u16::MAX) {
            let bytes: Vec<u8> = mesh.indices.iter().flat_map(|&i| (i as u16).to_le_bytes()).collect();
            buffers.add(&bytes, mesh.indices.len(), UNSIGNED_SHORT, "SCALAR", Some(ELEMENT_ARRAY_BUFFER), json!({}))
        } else {
            let bytes: Vec<u8> = mesh.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
            buffers.add(&bytes, mesh.indices.len(), UNSIGNED_INT, "SCALAR", Some(ELEMENT_ARRAY_BUFFER), json!({}))
        };
        let mut primitive = json!({ "attributes": attributes, "indices": indices, "mode": 4 });
        if let Some(path) = mesh.material.as_deref() {
            let index = material_paths.iter().position(|&p| p == path).unwrap_or_else(|| {
                material_paths.push(path);
                material_paths.len() - 1
            });
            primitive["material"] = json!(index);
        }
        meshes.push(json!({ "name": mesh.name, "primitives": [primitive], "extras": { "scale": model.scale, "source": source } }));
        let mut node = json!({ "name": mesh.name, "mesh": meshes.len() - 1 });
        if model.skeleton.is_some() {
            node["skin"] = json!(0);
        }
        nodes.push(node);
    }
    let mut scene_nodes: Vec<usize> = (0..nodes.len()).collect();

    let mut skins = Vec::new();
    if let Some(skeleton) = &model.skeleton {
        let first_bone = nodes.len();
        for (i, bone) in skeleton.bones.iter().enumerate() {
            let children: Vec<usize> = skeleton.bones.iter().enumerate().filter(|(_, b)| b.parent == Some(i)).map(|(child, _)| first_bone + child).collect();
            let mut node = json!({ "name": bone.name, "rotation": bone.rotation, "translation": bone.translation });
            if !children.is_empty() {
                node["children"] = json!(children);
            }
            if bone.parent.is_none() {
                scene_nodes.push(first_bone + i);
            }
            nodes.push(node);
        }
        let matrices = buffers.floats(&skeleton.inverse_binds, "MAT4", None, json!({}));
        let joints: Vec<usize> = skeleton.joints.iter().map(|&bone| first_bone + bone).collect();
        skins.push(json!({ "name": "Skeleton", "joints": joints, "inverseBindMatrices": matrices }));
    }

    let materials: Vec<Value> = material_paths
        .iter()
        .map(|path| json!({ "name": path.rsplit('/').next().unwrap_or(path), "doubleSided": true, "extras": { "path": path } }))
        .collect();
    while !buffers.bin.len().is_multiple_of(4) {
        buffers.bin.push(0);
    }
    let mut document = json!({
        "asset": { "version": "2.0", "generator": concat!("wfscan ", env!("CARGO_PKG_VERSION")) },
        "scene": 0,
        "scenes": [{ "nodes": scene_nodes }],
        "nodes": nodes,
        "meshes": meshes,
        "accessors": buffers.accessors,
        "bufferViews": buffers.views,
        "buffers": [{ "byteLength": buffers.bin.len() }],
    });
    if !materials.is_empty() {
        document["materials"] = json!(materials);
    }
    if !skins.is_empty() {
        document["skins"] = json!(skins);
    }

    let mut text = serde_json::to_vec(&document).expect("a JSON value serialises");
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    let total = 12 + 8 + text.len() + 8 + buffers.bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&text);
    out.extend_from_slice(&(buffers.bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&buffers.bin);
    out
}

/// Split a `.glb` file into its JSON document and binary chunk. For reading back what
/// [`write`] produced.
pub fn parse(glb: &[u8]) -> Option<(Value, &[u8])> {
    let u32_at = |at: usize| Some(u32::from_le_bytes(glb.get(at..at + 4)?.try_into().ok()?) as usize);
    if glb.get(..4)? != b"glTF" || u32_at(4)? != 2 || u32_at(8)? != glb.len() {
        return None;
    }
    let json_len = u32_at(12)?;
    if glb.get(16..20)? != b"JSON" {
        return None;
    }
    let document = serde_json::from_slice(glb.get(20..20 + json_len)?).ok()?;
    let bin_at = 20 + json_len;
    let bin_len = u32_at(bin_at)?;
    if glb.get(bin_at + 4..bin_at + 8)? != b"BIN\0" {
        return None;
    }
    Some((document, glb.get(bin_at + 8..bin_at + 8 + bin_len)?))
}

#[cfg(test)]
mod tests {
    use super::super::model::{Bone, Mesh, Skeleton};
    use super::*;

    fn quad() -> Model {
        Model {
            positions: vec![[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 3.0, -1.0], [0.0, 3.0, 1.0]],
            uv0: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            uv1: vec![[0.5; 2]; 4],
            occlusion: vec![255, 128, 64, 0],
            joints: vec![],
            weights: vec![],
            meshes: vec![
                Mesh { name: "Front".into(), material: Some("/Lotus/A/FrontMat".into()), indices: vec![0, 1, 2] },
                Mesh { name: "Back".into(), material: None, indices: vec![0, 2, 3] },
                Mesh { name: "Again".into(), material: Some("/Lotus/A/FrontMat".into()), indices: vec![0, 1, 3] },
            ],
            skeleton: None,
            scale: [2.0, 3.0, 1.0],
        }
    }

    /// The bytes an accessor covers.
    fn accessor_bytes<'a>(document: &Value, bin: &'a [u8], accessor: &Value) -> &'a [u8] {
        let accessor = &document["accessors"][accessor.as_u64().unwrap() as usize];
        let view = &document["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
        let (offset, length) = (view["byteOffset"].as_u64().unwrap() as usize, view["byteLength"].as_u64().unwrap() as usize);
        &bin[offset..offset + length]
    }

    #[test]
    fn a_static_model() {
        let glb = write(&quad(), "/Lotus/A/Quad.fbx");
        assert_eq!(glb.len() % 4, 0);
        let (document, bin) = parse(&glb).unwrap();
        assert_eq!(document["asset"]["version"], "2.0");
        assert_eq!(document["buffers"][0]["byteLength"], bin.len());
        assert_eq!(document["scenes"][0]["nodes"], json!([0, 1, 2]));
        assert!(document.get("skins").is_none());

        let meshes = document["meshes"].as_array().unwrap();
        assert_eq!(meshes.iter().map(|m| m["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Front", "Back", "Again"]);
        assert_eq!(meshes[0]["extras"], json!({ "scale": [2.0, 3.0, 1.0], "source": "/Lotus/A/Quad.fbx" }));
        let primitives: Vec<&Value> = meshes.iter().map(|m| &m["primitives"][0]).collect();
        assert_eq!((&primitives[0]["material"], primitives[1].get("material"), &primitives[2]["material"]), (&json!(0), None, &json!(0)));
        assert_eq!(document["materials"], json!([{ "name": "FrontMat", "doubleSided": true, "extras": { "path": "/Lotus/A/FrontMat" } }]));

        let attributes = &primitives[0]["attributes"];
        assert_eq!(attributes, &primitives[2]["attributes"], "meshes share the vertex data");
        let position = &document["accessors"][attributes["POSITION"].as_u64().unwrap() as usize];
        assert_eq!((position["count"].as_u64(), position["type"].as_str()), (Some(4), Some("VEC3")));
        assert_eq!((&position["min"], &position["max"]), (&json!([0.0, 0.0, -1.0]), &json!([2.0, 3.0, 1.0])));
        let bytes = accessor_bytes(&document, bin, &attributes["POSITION"]);
        assert_eq!(bytes.len(), 48);
        assert_eq!(f32::from_le_bytes(bytes[36..40].try_into().unwrap()), 0.0);
        assert_eq!(f32::from_le_bytes(bytes[44..48].try_into().unwrap()), 1.0);
        assert_eq!(accessor_bytes(&document, bin, &attributes["_OCCLUSION"]), [255, 128, 64, 0]);
        let back = accessor_bytes(&document, bin, &primitives[1]["indices"]);
        assert_eq!(back, [0, 0, 2, 0, 3, 0], "16-bit indices for a small model");

        // Every view lies inside the buffer and starts on a 4-byte boundary.
        for view in document["bufferViews"].as_array().unwrap() {
            let (offset, length) = (view["byteOffset"].as_u64().unwrap(), view["byteLength"].as_u64().unwrap());
            assert!(offset % 4 == 0 && offset + length <= bin.len() as u64, "{view}");
        }
    }

    #[test]
    fn a_rigged_model() {
        let mut model = quad();
        model.joints = vec![[0, 1, 0, 0]; 4];
        model.weights = vec![[0.75, 0.25, 0.0, 0.0]; 4];
        let bone = |name: &str, parent| Bone { name: name.into(), parent, rotation: [0.0, 0.0, 0.0, 1.0], translation: [1.0, 2.0, 3.0] };
        model.skeleton = Some(Skeleton {
            bones: vec![bone("Root", None), bone("Spine", Some(0)), bone("Arm", Some(1)), bone("Prop", None)],
            joints: vec![2, 0, 1, 3],
            inverse_binds: vec![[1.0; 16]; 4],
        });
        let glb = write(&model, "/Lotus/A/Quad_skel.fbx");
        let (document, bin) = parse(&glb).unwrap();
        let nodes = document["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 3 + 4);
        assert_eq!((&nodes[0]["mesh"], &nodes[0]["skin"]), (&json!(0), &json!(0)));
        assert_eq!((nodes[3]["name"].as_str(), &nodes[3]["children"]), (Some("Root"), &json!([4])));
        assert_eq!((&nodes[4]["children"], nodes[5].get("children")), (&json!([5]), None));
        assert_eq!(nodes[4]["translation"], json!([1.0, 2.0, 3.0]));
        assert_eq!(document["scenes"][0]["nodes"], json!([0, 1, 2, 3, 6]), "meshes, then each root bone");
        let skin = &document["skins"][0];
        assert_eq!(skin["joints"], json!([5, 3, 4, 6]));
        let matrices = &document["accessors"][skin["inverseBindMatrices"].as_u64().unwrap() as usize];
        assert_eq!((matrices["count"].as_u64(), matrices["type"].as_str()), (Some(4), Some("MAT4")));
        let attributes = &document["meshes"][0]["primitives"][0]["attributes"];
        assert_eq!(accessor_bytes(&document, bin, &attributes["JOINTS_0"])[..8], [0, 0, 1, 0, 0, 0, 0, 0]);
        assert_eq!(accessor_bytes(&document, bin, &attributes["WEIGHTS_0"]).len(), 64);
    }

    #[test]
    fn large_models_get_32_bit_indices() {
        let mut model = quad();
        model.positions = vec![[0.0; 3]; 70_000];
        model.uv0 = vec![[0.0; 2]; 70_000];
        model.uv1 = vec![[0.0; 2]; 70_000];
        model.occlusion = vec![0; 70_000];
        model.meshes = vec![Mesh { name: "Big".into(), material: None, indices: vec![0, 69_999, 5] }];
        let glb = write(&model, "/A/Big.fbx");
        let (document, bin) = parse(&glb).unwrap();
        let indices = &document["meshes"][0]["primitives"][0]["indices"];
        assert_eq!(document["accessors"][indices.as_u64().unwrap() as usize]["componentType"], UNSIGNED_INT);
        assert_eq!(accessor_bytes(&document, bin, indices)[4..8], 69_999u32.to_le_bytes());
    }

    #[test]
    fn parse_refuses_what_is_not_glb() {
        let glb = write(&quad(), "/A/Q.fbx");
        assert!(parse(&glb[..glb.len() - 1]).is_none());
        assert!(parse(b"not a glb file at all").is_none());
    }
}
