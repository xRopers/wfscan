//! A model as a standard tool sees it: vertices in units, one index list per mesh, bone
//! indices that mean joints of a skeleton.
//!
//! What this undoes of the game's storage:
//! - Packed positions are fractions of a per-axis scale: the larger magnitude of the
//!   header's two scale vectors (an axis with scale 0 is left alone).
//! - Each mesh has five levels of detail in one index list; the finest is kept. Models
//!   with morphs need each mesh's vertex offset added to its indices.
//! - A vertex's bone indices are positions in its mesh's bone map, which gives positions
//!   in the header's list of weighted bones. Those become skin joints, followed by the
//!   bones nothing is weighted to.
//! - The game's axes are mirrored against glTF's. Static models have Z negated. Rigged
//!   models keep their vertices and have the skeleton mirrored in X instead (bone
//!   positions, rotations and inverse bind matrices), as the original does.
//!
//! Bone rotations are stored x, y, z, w: with that order each weighted bone's world
//! transform times its inverse bind matrix is the identity to within 0.06 on average
//! over 6,633 bones of the September 2026 cache (3.2 with w first).
//!
//! Not converted: normals and tangents. The four normal bytes don't decode to unit
//! vectors, or to anything that lines up with the triangles, under any reading tried
//! (plain bytes, 10-bit fields, a tangent-frame quaternion), so models are written
//! without normals and viewers compute their own. Triangle winding is unverified for the
//! same reason; materials are written double-sided.
//!
//! Sources: the scale rule, level of detail, morph offsets, bone maps and mirroring
//! follow the original's description; the rotation order was measured.

use std::collections::HashMap;

use super::raw::{ModelBody, ModelHeader, Variant};

#[derive(Debug, Clone, PartialEq)]
pub struct Mesh {
    pub name: String,
    /// Internal path of the mesh's material, if the file names one.
    pub material: Option<String>,
    /// Triangles, three vertex indices each.
    pub indices: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    /// Relative to the parent: a quaternion as x, y, z, w, and a position.
    pub rotation: [f32; 4],
    pub translation: [f32; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Skeleton {
    pub bones: Vec<Bone>,
    /// The bone each skin joint is: weighted bones first, in the order vertices refer to
    /// them, then every other bone.
    pub joints: Vec<usize>,
    /// Per joint, column-major.
    pub inverse_binds: Vec<[f32; 16]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub positions: Vec<[f32; 3]>,
    pub uv0: Vec<[f32; 2]>,
    pub uv1: Vec<[f32; 2]>,
    /// Baked ambient occlusion, 0-255.
    pub occlusion: Vec<u8>,
    /// Per vertex, for rigged models: four joints of the skeleton and their weights.
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub meshes: Vec<Mesh>,
    pub skeleton: Option<Skeleton>,
    /// What packed positions were multiplied by.
    pub scale: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    #[error("the model has no meshes")]
    NoMeshes,
    #[error("mesh {mesh}: its triangles are indices {start}..{end} of {total}")]
    MeshRange { mesh: String, start: u64, end: u64, total: usize },
    #[error("mesh {mesh}: its index count {count} is not a multiple of 3")]
    NotTriangles { mesh: String, count: u32 },
    #[error("mesh {mesh}: vertex index {index} of {vertices} vertices")]
    VertexIndex { mesh: String, index: u64, vertices: usize },
    #[error("bone {bone}: its parent is bone {parent} of {bones}")]
    BoneParent { bone: String, parent: u16, bones: usize },
    #[error("a vertex is weighted to joint {joint} of {joints}")]
    Joint { joint: u32, joints: usize },
}

/// The material names in a model's attribute text:
/// ```text
/// Materials={
///     /Lotus/Characters/A/BodyMat,
///     HeadMat
/// }
/// ```
pub fn material_names(attributes: &str) -> Vec<String> {
    let Some((_, rest)) = attributes.split_once("Materials={") else { return Vec::new() };
    let list = rest.split_once('}').map_or(rest, |(list, _)| list);
    list.lines().map(|line| line.trim().trim_end_matches(',').trim_matches('"')).filter(|name| !name.is_empty()).map(str::to_string).collect()
}

/// A material name as a full internal path: relative names are next to the model.
fn material_path(name: &str, model_path: &str) -> String {
    if name.starts_with('/') {
        return name.to_string();
    }
    let folder = model_path.rsplit_once('/').map_or("", |(folder, _)| folder);
    format!("{folder}/{name}")
}

impl Model {
    /// Convert a model read by [`super::raw`]. `attributes` is the common header's text
    /// and `path` the model's internal path, both for material names.
    pub fn build(variant: Variant, header: &ModelHeader, body: ModelBody, attributes: &str, path: &str) -> Result<Model, ModelError> {
        if header.mesh_infos.is_empty() {
            return Err(ModelError::NoMeshes);
        }
        let vertices = body.positions.len();
        let scale = if variant.positions_are_scaled() {
            [0, 1, 2].map(|axis| match header.scale1[axis].abs().max(header.scale2[axis].abs()) {
                0.0 => 1.0,
                scale => scale,
            })
        } else {
            [1.0; 3]
        };
        // Static models are mirrored here; rigged ones through their skeleton.
        let mirror_z = if variant.is_rigged() { 1.0 } else { -1.0 };
        let positions = body.positions.iter().map(|p| [p[0] * scale[0], p[1] * scale[1], p[2] * scale[2] * mirror_z]).collect();

        let names = if header.material_paths.is_empty() { material_names(attributes) } else { header.material_paths.clone() };
        let mut meshes = Vec::with_capacity(header.mesh_infos.len());
        for (i, info) in header.mesh_infos.iter().enumerate() {
            let (start, count) = (u64::from(info.lod_offsets[0]), info.lod_counts[0]);
            let end = start + u64::from(count);
            let range = usize::try_from(start).ok().zip(usize::try_from(end).ok()).and_then(|(s, e)| body.indices.get(s..e));
            let range = range.ok_or_else(|| ModelError::MeshRange { mesh: info.name.clone(), start, end, total: body.indices.len() })?;
            if !count.is_multiple_of(3) {
                return Err(ModelError::NotTriangles { mesh: info.name.clone(), count });
            }
            let offset = if header.morph_count > 0 { u64::from(info.lod_vertex_offsets[0]) } else { 0 };
            let indices = range
                .iter()
                .map(|&index| u64::from(index) + offset)
                .map(|index| u32::try_from(index).ok().filter(|&i| (i as usize) < vertices).ok_or(ModelError::VertexIndex { mesh: info.name.clone(), index, vertices }))
                .collect::<Result<Vec<u32>, _>>()?;
            let material = names.get(i).filter(|name| !name.is_empty() && *name != "None").map(|name| material_path(name, path));
            meshes.push(Mesh { name: info.name.clone(), material, indices });
        }

        let skeleton = if variant.is_rigged() && !header.bone_tree.is_empty() { Some(skeleton(header, &body)?) } else { None };
        let (mut joints, mut weights) = (Vec::new(), Vec::new());
        if let Some(skeleton) = &skeleton {
            // Each mesh's bone map turns its vertices' local bone indices into joints.
            joints = vec![[0u16; 4]; vertices];
            for (mesh, map) in meshes.iter().zip(&header.bone_maps) {
                for &vertex in &mesh.indices {
                    let vertex = vertex as usize;
                    for (slot, out) in joints[vertex].iter_mut().enumerate() {
                        if body.bone_weights[vertex][slot] == 0.0 {
                            continue;
                        }
                        // A local index past the end of this mesh's map belongs to
                        // another mesh sharing the vertex, which sets it.
                        let Some(&joint) = map.get(usize::from(body.bone_indices[vertex][slot])) else { continue };
                        if joint as usize >= skeleton.joints.len() {
                            return Err(ModelError::Joint { joint, joints: skeleton.joints.len() });
                        }
                        *out = joint as u16;
                    }
                }
            }
            weights = body.bone_weights;
        }
        Ok(Model { positions, uv0: body.uv0, uv1: body.uv1, occlusion: body.occlusion, joints, weights, meshes, skeleton, scale })
    }

    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|m| m.indices.len() / 3).sum()
    }
}

const IDENTITY: [f32; 16] = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

fn skeleton(header: &ModelHeader, body: &ModelBody) -> Result<Skeleton, ModelError> {
    let tree = &header.bone_tree;
    // Some layouts number parents from 1; the root then names itself.
    let shift = u16::from(tree.len() >= 2 && tree[1].parent == 1);
    let mut bones = Vec::with_capacity(tree.len());
    for (i, node) in tree.iter().enumerate() {
        let parent = node.parent.wrapping_sub(shift);
        let parent = match usize::from(parent) {
            p if p == i || node.parent < shift => None,
            p if p < tree.len() => Some(p),
            _ => return Err(ModelError::BoneParent { bone: node.name.clone(), parent: node.parent, bones: tree.len() }),
        };
        // Mirrored in X: positions lose their x sign, and a rotation seen in the mirror
        // turns the other way about y and z.
        let [x, y, z, w] = body.bone_rotations[i];
        let [tx, ty, tz] = body.bone_positions[i];
        bones.push(Bone { name: node.name.clone(), parent, rotation: [x, -y, -z, w], translation: [-tx, ty, tz] });
    }

    let by_name: HashMap<&str, usize> = tree.iter().enumerate().map(|(i, node)| (node.name.as_str(), i)).collect();
    let mut joints = Vec::with_capacity(tree.len());
    let mut inverse_binds = Vec::with_capacity(tree.len());
    let mut is_joint = vec![false; tree.len()];
    for (slot, (name, rows)) in header.weighted_bones.iter().zip(&body.inverse_binds).enumerate() {
        // A weighted bone that the tree doesn't have (often an empty name), or has given
        // to an earlier slot, still needs a joint of its own for the slots to line up:
        // it gets a bone at the root.
        let bone = match by_name.get(name.as_str()) {
            Some(&bone) if !std::mem::replace(&mut is_joint[bone], true) => bone,
            _ => {
                let name = if name.is_empty() { format!("unnamed_{slot}") } else { format!("{name}_{slot}") };
                bones.push(Bone { name, parent: None, rotation: [0.0, 0.0, 0.0, 1.0], translation: [0.0; 3] });
                bones.len() - 1
            }
        };
        joints.push(bone);
        // Three rows of four in the file; column-major here, with the first row negated:
        // the skeleton is mirrored and the vertices are not, so skinning mirrors them.
        let mut matrix = IDENTITY;
        for row in 0..3 {
            for column in 0..4 {
                matrix[column * 4 + row] = rows[row * 4 + column] * if row == 0 { -1.0 } else { 1.0 };
            }
        }
        inverse_binds.push(matrix);
    }
    for bone in (0..tree.len()).filter(|&bone| !is_joint[bone]) {
        joints.push(bone);
        inverse_binds.push(IDENTITY);
    }
    Ok(Skeleton { bones, joints, inverse_binds })
}

#[cfg(test)]
mod tests {
    use super::super::raw::{BoneNode, MeshInfo};
    use super::*;

    fn mesh_info(name: &str, offset: u32, count: u32) -> MeshInfo {
        MeshInfo { vector1: [0.0; 4], vector2: [0.0; 4], name: name.into(), lod_offsets: [offset, 0, 0, 0, 0], lod_counts: [count, 0, 0, 0, 0], lod_vertex_offsets: [0; 5] }
    }

    fn quad_body() -> ModelBody {
        ModelBody {
            positions: vec![[0.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0], [1.0, 1.0, 0.5, 0.0], [0.0, 1.0, -0.5, 0.0]],
            normals: vec![[0; 4]; 4],
            tangents: vec![[0; 3]; 4],
            occlusion: vec![255, 200, 100, 0],
            uv0: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            uv1: vec![[0.0; 2]; 4],
            indices: vec![0, 1, 2, 0, 2, 3],
            ..Default::default()
        }
    }

    #[test]
    fn material_names_from_attributes() {
        let attributes = "Something=1\nMaterials={\n    /Lotus/A/BodyMat,\n    HeadMat\n}\nOther={\n    x\n}\n";
        assert_eq!(material_names(attributes), ["/Lotus/A/BodyMat", "HeadMat"]);
        assert_eq!(material_names("no materials here"), Vec::<String>::new());
        assert_eq!(material_path("HeadMat", "/Lotus/A/B_skel.fbx"), "/Lotus/A/HeadMat");
        assert_eq!(material_path("/Lotus/X/Mat", "/Lotus/A/B_skel.fbx"), "/Lotus/X/Mat");
    }

    #[test]
    fn static_models_are_scaled_and_mirrored() {
        let header = ModelHeader {
            scale1: [2.0, -4.0, 0.0, 0.0],
            scale2: [-3.0, 1.0, 0.0, 0.0],
            vertex_count: 4,
            index_count: 6,
            mesh_infos: vec![mesh_info("Front", 0, 3), mesh_info("Back", 3, 3)],
            ..Default::default()
        };
        let m = Model::build(Variant::Static102, &header, quad_body(), "Materials={\n    FrontMat,\n    None\n}\n", "/Lotus/A/Quad.fbx").unwrap();
        assert_eq!(m.scale, [3.0, 4.0, 1.0]);
        assert_eq!(m.positions, [[0.0, 0.0, 0.0], [3.0, 0.0, 0.0], [3.0, 4.0, -0.5], [0.0, 4.0, 0.5]]);
        assert_eq!(m.meshes.len(), 2);
        assert_eq!((m.meshes[0].name.as_str(), m.meshes[0].material.as_deref(), &m.meshes[0].indices[..]), ("Front", Some("/Lotus/A/FrontMat"), &[0, 1, 2][..]));
        assert_eq!((m.meshes[1].material.as_deref(), &m.meshes[1].indices[..]), (None, &[0, 2, 3][..]));
        assert_eq!((m.triangle_count(), m.skeleton.is_none(), m.joints.is_empty()), (2, true, true));

        // Level models are already in units, and name their own materials.
        let level = ModelHeader { material_paths: vec!["/Lotus/L/WallMat".into()], mesh_infos: vec![mesh_info("Wall", 0, 6)], ..header.clone() };
        let m = Model::build(Variant::Level109, &level, quad_body(), "Materials={\n    Ignored\n}\n", "/Lotus/L/0_c.fbx").unwrap();
        assert_eq!((m.scale, m.positions[2]), ([1.0; 3], [1.0, 1.0, -0.5]));
        assert_eq!(m.meshes[0].material.as_deref(), Some("/Lotus/L/WallMat"));
    }

    #[test]
    fn bad_ranges_and_indices_are_refused() {
        let header = |infos: Vec<MeshInfo>| ModelHeader { mesh_infos: infos, ..Default::default() };
        let build = |h: &ModelHeader, body: ModelBody| Model::build(Variant::Static102, h, body, "", "/A/B.fbx");
        assert_eq!(build(&header(vec![]), quad_body()), Err(ModelError::NoMeshes));
        assert!(matches!(build(&header(vec![mesh_info("M", 3, 6)]), quad_body()), Err(ModelError::MeshRange { start: 3, end: 9, total: 6, .. })));
        assert!(matches!(build(&header(vec![mesh_info("M", 0, 4)]), quad_body()), Err(ModelError::NotTriangles { count: 4, .. })));
        let mut body = quad_body();
        body.indices[4] = 4;
        assert!(matches!(build(&header(vec![mesh_info("M", 0, 6)]), body), Err(ModelError::VertexIndex { index: 4, vertices: 4, .. })));

        // Morph models add a per-mesh offset to their indices.
        let mut info = mesh_info("M", 0, 3);
        info.lod_vertex_offsets[0] = 1;
        let morphed = ModelHeader { morph_count: 2, mesh_infos: vec![info], ..Default::default() };
        assert_eq!(build(&morphed, quad_body()).unwrap().meshes[0].indices, [1, 2, 3]);
    }

    #[test]
    fn rigged_models_get_a_mirrored_skeleton_and_joint_indices() {
        let bone = |name: &str, parent: u16| BoneNode { name: name.into(), parent };
        let header = ModelHeader {
            scale1: [1.0; 4],
            vertex_count: 4,
            index_count: 6,
            weighted_bones: vec!["Arm".into(), "Root".into()],
            bone_tree: vec![bone("Root", 0), bone("Spine", 0), bone("Arm", 1)],
            mesh_infos: vec![mesh_info("Body", 0, 6)],
            // Local bone 0 is weighted bone 1 (Root), local 1 is weighted bone 0 (Arm).
            bone_maps: vec![vec![1, 0]],
            ..Default::default()
        };
        let mut body = quad_body();
        body.bone_indices = vec![[0, 1, 9, 9], [1, 0, 0, 0], [0, 0, 0, 0], [1, 1, 1, 1]];
        body.bone_weights = vec![[0.5, 0.5, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0], [0.25, 0.25, 0.25, 0.25]];
        body.inverse_binds = vec![[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0], [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]];
        body.bone_rotations = vec![[0.0, 0.0, 0.0, 1.0], [0.1, 0.2, 0.3, 0.9], [0.0, 0.0, 0.0, 1.0]];
        body.bone_positions = vec![[0.0, 0.0, 0.0], [1.0, 2.0, 3.0], [0.5, 0.0, 0.0]];

        let m = Model::build(Variant::Rigged281, &header, body, "", "/A/B_skel.fbx").unwrap();
        assert_eq!(m.positions[2], [1.0, 1.0, 0.5], "rigged vertices are not mirrored themselves");
        let s = m.skeleton.as_ref().unwrap();
        assert_eq!(s.bones.iter().map(|b| (b.name.as_str(), b.parent)).collect::<Vec<_>>(), [("Root", None), ("Spine", Some(0)), ("Arm", Some(1))]);
        assert_eq!((s.bones[1].rotation, s.bones[1].translation), ([0.1, -0.2, -0.3, 0.9], [-1.0, 2.0, 3.0]));
        assert_eq!(s.joints, [2, 0, 1], "weighted bones in their order, then the rest");
        assert_eq!(s.inverse_binds[0], [-1.0, 5.0, 9.0, 0.0, -2.0, 6.0, 10.0, 0.0, -3.0, 7.0, 11.0, 0.0, -4.0, 8.0, 12.0, 1.0]);
        assert_eq!(s.inverse_binds[2], IDENTITY);
        assert_eq!(m.joints, [[1, 0, 0, 0], [0, 0, 0, 0], [1, 0, 0, 0], [0, 0, 0, 0]]);
        assert_eq!(m.weights[3], [0.25; 4]);

        // Weighted bones the tree lacks, or lists twice, get bones of their own at the
        // root, so that joint numbers still match what the vertices say.
        let mut odd = header.clone();
        odd.weighted_bones = vec!["Tail".into(), "".into(), "Root".into(), "Root".into()];
        let mut body = quad_body();
        body.bone_indices = vec![[0; 4]; 4];
        body.bone_weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
        body.inverse_binds = vec![[0.0; 12]; 4];
        body.bone_rotations = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        body.bone_positions = vec![[0.0; 3]; 3];
        let m = Model::build(Variant::Rigged281, &odd, body, "", "/A/B_skel.fbx").unwrap();
        let s = m.skeleton.unwrap();
        assert_eq!(s.bones.iter().skip(3).map(|b| (b.name.as_str(), b.parent)).collect::<Vec<_>>(), [("Tail_0", None), ("unnamed_1", None), ("Root_3", None)]);
        assert_eq!(s.joints, [3, 4, 0, 5, 1, 2]);
        assert_eq!(s.inverse_binds.len(), 6);
    }
}
