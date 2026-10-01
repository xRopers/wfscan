//! Model files as they are stored: the header in H, vertices and indices in B and F.
//!
//! A model's type number picks a header layout, and for some numbers so does its name:
//! `_dcm` and `_hlod` files share type numbers with plain models but are laid out
//! differently. Every layout is a fixed run of skips around the same building blocks
//! (mesh infos, bone tree, PhysX mesh list, ...), so each is written here as that run.
//!
//! The body starts in B with the PhysX data the header lists, then the skeleton (rigged
//! models), then vertices and 16-bit indices. Since update 40 the tail of both the
//! vertices and the indices can continue in F.
//!
//! Vertices come in three layouts:
//! - packed, 24 bytes: position as four i16 (divide by 32767; the header's scale turns
//!   that into units), normal as four u8, tangent as three u8, one u8 of ambient
//!   occlusion, two UV sets as half floats;
//! - rigged, 32 bytes: the same, then four u8 bone indices and a u32 holding three
//!   10-bit weights (the fourth is what is left of 1);
//! - level, 32 bytes: position as three f32 and 4 unused bytes, then normal, tangent,
//!   occlusion and UVs as above.
//!
//! Source: the original's model patterns and its description of each layout, for the
//! variants a September 2026 Warframe cache uses most (102, 109, 281 and the `_dcm` and
//! `_hlod` forms of 102 and 105).

use crate::formats::reader::{ReadError, Reader};

/// Which header and vertex layout a model file has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// Type 102: a static model, packed vertices, split between B and F.
    Static102,
    /// Types 102 and 105 named `*_dcm`.
    Dcm102,
    /// Types 102 and 105 named `*_hlod`.
    Hlod102,
    /// Type 109: a level's static model with float positions and its own material list.
    Level109,
    /// Type 281: a rigged model.
    Rigged281,
}

impl Variant {
    /// The variant for a Warframe model of this type and path, if wfscan reads it.
    pub fn of(type_id: u32, path: &str) -> Option<Self> {
        let name = path.rsplit('/').next().unwrap_or(path);
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        match (type_id, stem) {
            (102 | 105, s) if s.ends_with("_dcm") => Some(Self::Dcm102),
            (102 | 105, s) if s.ends_with("_hlod") => Some(Self::Hlod102),
            (_, s) if s.ends_with("_dcm") || s.ends_with("_hlod") || s.ends_with("_sm") => None,
            (102, _) => Some(Self::Static102),
            (109, _) => Some(Self::Level109),
            (281, _) => Some(Self::Rigged281),
            _ => None,
        }
    }

    pub fn is_rigged(self) -> bool {
        self == Self::Rigged281
    }

    /// Whether positions are fractions of the header's scale (as opposed to plain floats).
    pub fn positions_are_scaled(self) -> bool {
        self != Self::Level109
    }
}

/// One named part of a model, with where each level of detail's triangles are.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshInfo {
    pub vector1: [f32; 4],
    pub vector2: [f32; 4],
    pub name: String,
    /// Index of each LOD's first index, and how many indices it has. LOD 0 is the finest.
    pub lod_offsets: [u32; 5],
    pub lod_counts: [u32; 5],
    /// For models with morphs: what to add to each LOD's indices.
    pub lod_vertex_offsets: [u32; 5],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoneNode {
    pub name: String,
    pub parent: u16,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelHeader {
    /// Two vectors whose larger magnitudes per axis are the model's scale.
    pub scale1: [f32; 4],
    pub scale2: [f32; 4],
    pub vertex_count: u32,
    pub index_count: u32,
    pub morph_count: u32,
    pub bone_count: u32,
    /// How many of the vertices and indices are in B; the rest are in F.
    pub vertex_count_b: u32,
    pub index_count_b: u32,
    /// Bones that vertices are weighted to, in the order bone maps refer to them.
    pub weighted_bones: Vec<String>,
    pub bone_tree: Vec<BoneNode>,
    /// Bytes to skip in the body after the bind matrices, and 8-byte units after the
    /// vertices (morph data).
    pub body_skip1: u32,
    pub body_skip2: u32,
    pub mesh_infos: Vec<MeshInfo>,
    /// Per mesh: what each of its vertices' bone indices means, as an index into
    /// `weighted_bones`.
    pub bone_maps: Vec<Vec<u32>>,
    /// Per mesh, for level models. Other models name their materials in the common
    /// header's attributes.
    pub material_paths: Vec<String>,
    /// Bytes of PhysX data at the start of B, per PhysX mesh.
    pub physx_lengths: Vec<u32>,
    pub errors: Vec<String>,
}

/// The original's "physics struct": two u32s, two more bytes if either has bit 3 set
/// (`exact`: is exactly 8), and a path.
fn skip_physics(r: &mut Reader, exact: bool) -> Result<(), ReadError> {
    let (kind, sub) = (r.u32("physics type")?, r.u32("physics subtype")?);
    let extra = if exact { kind == 8 || sub == 8 } else { kind & 8 != 0 || sub & 8 != 0 };
    if extra {
        r.skip(2, "physics padding")?;
    }
    r.skip_string("physics path")
}

fn mesh_infos(r: &mut Reader) -> Result<Vec<MeshInfo>, ReadError> {
    (0..r.count(50, "mesh info count")?)
        .map(|_| {
            let (vector1, vector2) = (r.f32s("mesh info vector")?, r.f32s("mesh info vector")?);
            let name = r.string("mesh name")?;
            let mut lod = [[0u32; 5]; 2];
            for value in lod.iter_mut().flatten() {
                *value = r.u32("mesh LOD range")?;
            }
            r.skip(0x20, "mesh info")?;
            r.skip_string("mesh info name")?;
            r.skip(0x10, "mesh info")?;
            Ok(MeshInfo { vector1, vector2, name, lod_offsets: lod[0], lod_counts: lod[1], lod_vertex_offsets: [0; 5] })
        })
        .collect()
}

fn physx_lengths(r: &mut Reader) -> Result<Vec<u32>, ReadError> {
    (0..r.count(5000, "PhysX mesh count")?)
        .map(|_| {
            let kind = r.u32("PhysX type")?;
            r.skip(
                match kind {
                    1 => 0x4c,
                    7 => 0x60,
                    _ => 0x50,
                },
                "PhysX mesh",
            )?;
            r.skip(32, "PhysX vectors")?;
            if !matches!(kind, 0 | 2 | 3) {
                r.skip(4, "PhysX mesh")?;
            }
            r.skip(4, "PhysX mesh")?;
            let length = r.u32("PhysX data length")?;
            r.skip(8, "PhysX mesh")?;
            Ok(length)
        })
        .collect()
}

fn skip_array(r: &mut Reader, max: u32, item: usize, what: &'static str) -> Result<(), ReadError> {
    let count = r.count(max, what)?;
    r.skip(count * item, what)
}

/// The per-mesh morph tables: fills in each mesh's LOD vertex offsets and returns how
/// many 8-byte units of morph data follow the vertices in the body.
fn morph_tables(r: &mut Reader, meshes: &mut [MeshInfo]) -> Result<u32, ReadError> {
    let count = r.count(50, "morph table count")?;
    let mut skip = 0u32;
    for mesh in 0..count {
        let five = |r: &mut Reader| -> Result<[u32; 5], ReadError> {
            r.count(5, "morph table length")?;
            let mut values = [0; 5];
            for value in &mut values {
                *value = r.u32("morph table")?;
            }
            Ok(values)
        };
        let multipliers = five(r)?;
        let vertex_offsets = five(r)?;
        five(r)?;
        let bases = five(r)?;
        if let Some(info) = meshes.get_mut(mesh) {
            info.lod_vertex_offsets = vertex_offsets;
        }
        skip = skip.saturating_add(bases[0].saturating_mul(multipliers[0].max(1)));
        for _ in 0..r.count(1000, "morph name count")? {
            r.skip_string("morph name")?;
        }
        skip_array(r, 30, 4, "morph table tail")?;
    }
    Ok(skip)
}

/// Parse a model's header: the H part after the common header.
pub fn header(variant: Variant, data: &[u8]) -> Result<ModelHeader, ReadError> {
    let mut r = Reader::new(data);
    let r = &mut r;
    let mut h = ModelHeader::default();
    let counts = |r: &mut Reader, h: &mut ModelHeader| -> Result<(), ReadError> {
        h.scale1 = r.f32s("scale")?;
        h.scale2 = r.f32s("scale")?;
        h.vertex_count = r.u32("vertex count")?;
        h.index_count = r.u32("index count")?;
        h.morph_count = r.u32("morph count")?;
        h.bone_count = r.u32("bone count")?;
        Ok(())
    };
    r.skip(0x30, "header")?;
    match variant {
        Variant::Static102 => {
            skip_physics(r, true)?;
            r.skip(0x28, "header")?;
            counts(r, &mut h)?;
            r.skip(0x22, "header")?;
            h.vertex_count_b = r.u32("B vertex count")?;
            h.index_count_b = r.u32("B index count")?;
            r.skip(0x3d, "header")?;
            h.mesh_infos = mesh_infos(r)?;
            skip_array(r, 1000, 2, "u16 list")?;
            skip_physics(r, true)?;
            h.physx_lengths = physx_lengths(r)?;
            h.errors = r.strings(1000, "error list")?;
            r.finish()?;
            if h.vertex_count_b == 0 {
                (h.vertex_count_b, h.index_count_b) = (h.vertex_count, h.index_count);
            }
        }
        Variant::Dcm102 | Variant::Hlod102 => {
            skip_physics(r, false)?;
            r.skip(0x2c, "header")?;
            // Two bytes that are sometimes there: a small flag pair in front of the scale.
            if let Some(&[low, high]) = r.peek(0, 2).map(|b| <&[u8; 2]>::try_from(b).expect("two bytes"))
                && matches!(u16::from_le_bytes([low, high]), 0 | 256 | 257 | 258 | 260)
            {
                r.skip(2, "header")?;
            }
            counts(r, &mut h)?;
            r.skip(0x18, "header")?;
            skip_array(r, 10, 8, "u64 list")?;
            r.skip(0xc, "header")?;
            r.skip_string("path")?;
            r.skip(0x41, "header")?;
            h.mesh_infos = mesh_infos(r)?;
            skip_array(r, 1000, 2, "u16 list")?;
            if variant == Variant::Dcm102 {
                r.skip(1, "header")?;
                if r.u32("DCM kind")? == 2 {
                    r.skip(6, "header")?;
                }
            } else {
                r.skip(4, "header")?;
            }
            skip_physics(r, false)?;
            h.physx_lengths = physx_lengths(r)?;
            if variant == Variant::Dcm102 {
                h.errors = r.strings(1000, "error list")?;
                r.finish()?;
            }
            (h.vertex_count_b, h.index_count_b) = (h.vertex_count, h.index_count);
        }
        Variant::Level109 => {
            skip_physics(r, false)?;
            r.skip(0x28, "header")?;
            counts(r, &mut h)?;
            r.skip(0x67, "header")?;
            h.mesh_infos = mesh_infos(r)?;
            skip_array(r, 1000, 2, "u16 list")?;
            h.material_paths = r.strings(100, "material list")?;
            skip_physics(r, false)?;
            h.physx_lengths = physx_lengths(r)?;
            h.errors = r.strings(1000, "error list")?;
            r.finish()?;
            (h.vertex_count_b, h.index_count_b) = (h.vertex_count, h.index_count);
        }
        Variant::Rigged281 => {
            skip_physics(r, true)?;
            r.skip(0x2c, "header")?;
            h.scale1 = r.f32s("scale")?;
            h.scale2 = r.f32s("scale")?;
            h.weighted_bones = r.strings(1500, "weighted bone list")?;
            h.index_count = r.u32("index count")?;
            h.bone_count = r.u32("bone count")?;
            h.vertex_count = r.u32("vertex count")?;
            h.morph_count = r.u32("morph count")? & 0xffff;
            r.skip(0x13, "header")?;
            h.vertex_count_b = r.u32("B vertex count")?;
            h.index_count_b = r.u32("B index count")?;
            r.skip(0x25, "header")?;
            h.bone_tree = (0..r.count(1500, "bone tree size")?)
                .map(|_| {
                    let name = r.string("bone name")?;
                    let parent = r.u16("bone parent")?;
                    r.skip(2, "bone")?;
                    Ok(BoneNode { name, parent })
                })
                .collect::<Result<_, ReadError>>()?;
            let extra = r.count(5000, "attachment count")?;
            for _ in 0..extra {
                r.skip(0x10, "attachment")?;
                r.skip_string("attachment name")?;
                r.skip(0x4c, "attachment")?;
            }
            r.skip(0x1a, "header")?;
            h.body_skip1 = r.u32("body skip")?;
            r.skip(0x10 * extra + 8, "header")?;
            h.mesh_infos = mesh_infos(r)?;
            skip_array(r, 1000, 2, "u16 list")?;
            for _ in 0..r.count(5000, "morph count")? {
                r.skip_string("morph name")?;
                r.skip(4, "morph")?;
            }
            h.bone_maps = (0..r.count(500, "bone map count")?)
                .map(|_| (0..r.count(1500, "bone map size")?).map(|_| r.u32("bone map")).collect())
                .collect::<Result<_, ReadError>>()?;
            h.body_skip2 = morph_tables(r, &mut h.mesh_infos)?;
            r.skip(0x2f, "header")?;
            skip_physics(r, true)?;
            h.physx_lengths = physx_lengths(r)?;
            h.errors = r.strings(1000, "error list")?;
            r.finish()?;
            // Older files keep everything in B and leave the B counts at zero.
            if h.vertex_count_b == 0 {
                (h.vertex_count_b, h.index_count_b) = (h.vertex_count, h.index_count);
            }
        }
    }
    Ok(h)
}

/// A model's vertex and index data, still in the file's own terms.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelBody {
    /// As fractions of the scale (packed layouts) or in units (level layout); the fourth
    /// value is unused.
    pub positions: Vec<[f32; 4]>,
    pub normals: Vec<[u8; 4]>,
    pub tangents: Vec<[u8; 3]>,
    pub occlusion: Vec<u8>,
    pub uv0: Vec<[f32; 2]>,
    pub uv1: Vec<[f32; 2]>,
    /// Rigged models: indices into the vertex's mesh's bone map, and their weights.
    pub bone_indices: Vec<[u8; 4]>,
    pub bone_weights: Vec<[f32; 4]>,
    pub indices: Vec<u16>,
    /// Rigged models: per weighted bone, three rows of four: the top of its inverse bind
    /// matrix.
    pub inverse_binds: Vec<[f32; 12]>,
    /// Rigged models: per bone of the tree, four rotation values and a position.
    pub bone_rotations: Vec<[f32; 4]>,
    pub bone_positions: Vec<[f32; 3]>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BodyError {
    #[error("{part}: {source}")]
    Read { part: char, source: ReadError },
    #[error("the header puts {b} of {total} {what} in B")]
    Split { what: &'static str, b: u32, total: u32 },
}

fn vertices(r: &mut Reader, variant: Variant, count: u32, body: &mut ModelBody) -> Result<(), ReadError> {
    for _ in 0..count {
        let position = if variant.positions_are_scaled() {
            let mut p = [0.0; 4];
            for value in &mut p {
                *value = f32::from(r.i16("position")?) / 32767.0;
            }
            p
        } else {
            let p: [f32; 3] = r.f32s("position")?;
            r.skip(4, "position")?;
            [p[0], p[1], p[2], 0.0]
        };
        body.positions.push(position);
        body.normals.push(r.array("normal")?);
        body.tangents.push(r.array("tangent")?);
        body.occlusion.push(r.u8("occlusion")?);
        body.uv0.push([r.f16("UV")?, r.f16("UV")?]);
        body.uv1.push([r.f16("UV")?, r.f16("UV")?]);
        if variant.is_rigged() {
            body.bone_indices.push(r.array("bone indices")?);
            let packed = r.u32("bone weights")?;
            let three = [0, 10, 20].map(|shift| (packed >> shift & 0x3ff) as f32 / 1023.0);
            let rest = 1.0 - three.iter().sum::<f32>();
            // A vertex with no fourth bone must get exactly zero, not rounding noise.
            body.bone_weights.push([three[0], three[1], three[2], if rest > 0.0001 { rest } else { 0.0 }]);
        }
    }
    Ok(())
}

fn indices(r: &mut Reader, count: u32, body: &mut ModelBody) -> Result<(), ReadError> {
    let bytes = r.bytes(count as usize * 2, "indices")?;
    body.indices.extend(bytes.as_chunks::<2>().0.iter().map(|i| u16::from_le_bytes(*i)));
    Ok(())
}

/// How large a block of PhysX data some level models carry in front of their vertices.
const LEVEL_PHYSX_BLOCK: usize = 0x120;

fn body_b(r: &mut Reader, variant: Variant, h: &ModelHeader, body: &mut ModelBody) -> Result<(), ReadError> {
    for &length in &h.physx_lengths {
        r.skip(length as usize, "PhysX data")?;
    }
    match variant {
        Variant::Static102 => {}
        Variant::Dcm102 | Variant::Hlod102 => r.skip(64 * h.bone_count as usize, "bone data")?,
        Variant::Level109 => {
            r.skip(64 * h.bone_count as usize, "bone data")?;
            // More PhysX blocks, not counted anywhere: each ends in 16 zero bytes.
            while r.peek(0x100, 16).is_some_and(|tail| tail.iter().all(|&b| b == 0)) && r.remaining() >= LEVEL_PHYSX_BLOCK {
                r.skip(LEVEL_PHYSX_BLOCK, "PhysX block")?;
            }
        }
        Variant::Rigged281 => {}
    }
    r.skip(2, "body prefix")?;
    if variant == Variant::Rigged281 {
        for _ in 0..h.weighted_bones.len() {
            body.inverse_binds.push(r.f32s("inverse bind matrix")?);
        }
        r.skip(h.body_skip1 as usize, "body")?;
        for _ in 0..h.bone_tree.len() {
            body.bone_rotations.push(r.f32s("bone rotation")?);
            body.bone_positions.push(r.f32s("bone position")?);
            r.skip(12, "bone")?;
        }
        r.skip(4 * h.bone_tree.len(), "bone tables")?;
        if !h.bone_tree.is_empty() {
            r.skip(8, "bone tables")?;
        }
    }
    vertices(r, variant, h.vertex_count_b, body)?;
    match variant {
        Variant::Rigged281 => r.skip(h.body_skip2 as usize * 8, "morph data")?,
        // Some DCM models carry 8 more bytes per vertex before the indices.
        Variant::Dcm102 if r.remaining() > h.index_count as usize * 2 => r.skip(h.vertex_count as usize * 8, "extra vertex data")?,
        _ => {}
    }
    indices(r, h.index_count_b, body)?;
    // Not in the original's description: static models with a morph count carry 8 bytes
    // per morph after their indices (595 models of the September 2026 cache; with the
    // data placed before the indices instead, 548 of them index past their vertices).
    if variant == Variant::Static102 {
        r.skip(h.morph_count as usize * 8, "morph data")?;
    }
    // The layouts whose end is known must end here.
    if matches!(variant, Variant::Static102 | Variant::Rigged281) {
        r.finish()?;
    }
    Ok(())
}

/// Read a model's vertices and indices from its B and F parts (`f` empty if absent).
pub fn body(variant: Variant, h: &ModelHeader, b: &[u8], f: &[u8]) -> Result<ModelBody, BodyError> {
    for (what, in_b, total) in [("vertices", h.vertex_count_b, h.vertex_count), ("indices", h.index_count_b, h.index_count)] {
        if in_b > total {
            return Err(BodyError::Split { what, b: in_b, total });
        }
    }
    let mut body = ModelBody::default();
    let mut r = Reader::new(b);
    body_b(&mut r, variant, h, &mut body).map_err(|source| BodyError::Read { part: 'B', source })?;

    let (vertices_f, indices_f) = (h.vertex_count - h.vertex_count_b, h.index_count - h.index_count_b);
    let mut r = Reader::new(f);
    let rest = (|| {
        vertices(&mut r, variant, vertices_f, &mut body)?;
        indices(&mut r, indices_f, &mut body)?;
        // An F part holds nothing but the rest of the model.
        if vertices_f + indices_f > 0 { r.finish() } else { Ok(()) }
    })();
    rest.map_err(|source| BodyError::Read { part: 'F', source })?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_by_type_and_name() {
        assert_eq!(Variant::of(102, "/Lotus/Objects/Crate.fbx"), Some(Variant::Static102));
        assert_eq!(Variant::of(102, "/Lotus/Objects/Crate_dcm.fbx"), Some(Variant::Dcm102));
        assert_eq!(Variant::of(105, "/Lotus/Levels/Room/0_hlod.fbx"), Some(Variant::Hlod102));
        assert_eq!(Variant::of(102, "/Lotus/Levels/Room/Thing_hlod"), Some(Variant::Hlod102));
        assert_eq!(Variant::of(109, "/Lotus/Docs/Loadouts/0_c.fbx"), Some(Variant::Level109));
        assert_eq!(Variant::of(281, "/Lotus/Characters/Tenno/Excalibur/ExcaliburBody_skel.fbx"), Some(Variant::Rigged281));
        for (type_id, path) in [(108, "/A/B_dcm.fbx"), (105, "/A/B.fbx"), (106, "/A/B_sm.fbx"), (299, "/A/B.fbx"), (166, "/A/T.terrain")] {
            assert_eq!(Variant::of(type_id, path), None, "{type_id} {path}");
        }
    }

    #[test]
    fn weights_unpack_to_four() {
        // Weights 1023/1023, 0, 0 and 512, 511, 0 (of 1023).
        let mut data = Vec::new();
        for packed in [1023u32, 512 | 511 << 10, 256 | 256 << 10 | 256 << 20] {
            data.extend_from_slice(&[0; 24]);
            data.extend_from_slice(&[1, 2, 3, 4]);
            data.extend_from_slice(&packed.to_le_bytes());
        }
        let mut body = ModelBody::default();
        vertices(&mut Reader::new(&data), Variant::Rigged281, 3, &mut body).unwrap();
        assert_eq!(body.bone_weights[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(body.bone_weights[1][3], 0.0);
        assert!((body.bone_weights[1][0] + body.bone_weights[1][1] - 1.0).abs() < 1e-6);
        assert!((body.bone_weights[2][3] - (1.0 - 768.0 / 1023.0)).abs() < 1e-6);
        assert_eq!(body.bone_indices[2], [1, 2, 3, 4]);
    }

    #[test]
    fn packed_and_level_vertices() {
        let mut packed = Vec::new();
        for value in [32767i16, -32767, 0, 16384] {
            packed.extend_from_slice(&value.to_le_bytes());
        }
        packed.extend_from_slice(&[255, 128, 0, 9, 1, 2, 3, 77]);
        for half in [0x3c00u16, 0x3800, 0xbc00, 0x0000] {
            packed.extend_from_slice(&half.to_le_bytes());
        }
        let mut body = ModelBody::default();
        vertices(&mut Reader::new(&packed), Variant::Static102, 1, &mut body).unwrap();
        assert_eq!(body.positions[0][..3], [1.0, -1.0, 0.0]);
        assert_eq!((body.normals[0], body.tangents[0], body.occlusion[0]), ([255, 128, 0, 9], [1, 2, 3], 77));
        assert_eq!((body.uv0[0], body.uv1[0]), ([1.0, 0.5], [-1.0, 0.0]));
        assert!(body.bone_indices.is_empty());

        let mut level = Vec::new();
        for value in [1.5f32, -2.0, 300.0, 999.0] {
            level.extend_from_slice(&value.to_le_bytes());
        }
        level.extend_from_slice(&packed[8..]);
        let mut body = ModelBody::default();
        vertices(&mut Reader::new(&level), Variant::Level109, 1, &mut body).unwrap();
        assert_eq!(body.positions[0], [1.5, -2.0, 300.0, 0.0]);
        assert_eq!(body.uv0[0], [1.0, 0.5]);
    }

    #[test]
    fn a_split_larger_than_the_whole_is_refused() {
        let h = ModelHeader { vertex_count: 3, vertex_count_b: 4, ..Default::default() };
        assert_eq!(body(Variant::Static102, &h, &[], &[]), Err(BodyError::Split { what: "vertices", b: 4, total: 3 }));
    }
}
