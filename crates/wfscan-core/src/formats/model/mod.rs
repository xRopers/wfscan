//! Models: static and rigged meshes from the `Misc` packages, exported as binary glTF.
//!
//! Read so far: Warframe's types 102 (static), 109 (level) and 281 (rigged), and the
//! `_dcm` and `_hlod` forms of 102 and 105. On the September 2026 cache that is 53,131 of
//! 57,698 models. The rest (plain 105, 108, 166 terrain, 299 and Soulframe's types) are
//! reported as unsupported.

pub mod glb;
#[allow(clippy::module_inception)]
pub mod model;
pub mod raw;
pub mod render;

use crate::cache::package::{Game, Part};
use crate::export::{ExportError, ExportInput, Exporter, Output};
use crate::types::Kind;

pub use model::{Bone, Mesh, Model, ModelError, Skeleton};
pub use raw::Variant;

/// Read one model: its header from H, its vertices and indices from B and F.
pub fn read(input: &ExportInput) -> Result<Model, ExportError> {
    if input.game != Game::Warframe {
        return Err(ExportError::unsupported(format!("no model reader for {} yet", input.game)));
    }
    let variant = Variant::of(input.type_id(), input.path)
        .ok_or_else(|| ExportError::unsupported(format!("no reader for model type {} with this name yet", input.type_id())))?;
    let header = raw::header(variant, input.h_body()).map_err(|e| ExportError::failed(format!("header: {e}")))?;
    // Collision-only files: PhysX data and no meshes to draw.
    if header.mesh_infos.is_empty() || header.vertex_count == 0 {
        return Err(ExportError::unsupported("the model has no meshes (collision data only)"));
    }
    if !input.has(Part::B) {
        return Err(ExportError::failed("the model has no B part"));
    }
    let b = input.read(Part::B)?;
    let f = if input.has(Part::F) { input.read(Part::F)? } else { Vec::new() };
    let body = raw::body(variant, &header, &b, &f).map_err(|e| ExportError::failed(e.to_string()))?;
    Model::build(variant, &header, body, &input.header.attributes, input.path).map_err(|e| ExportError::failed(e.to_string()))
}

/// `/A/B_skel.fbx` as `/A/B_skel.glb`.
pub fn output_path(path: &str) -> String {
    let stem = match path.rsplit_once('.') {
        Some((stem, extension)) if !extension.contains('/') && extension.eq_ignore_ascii_case("fbx") => stem,
        _ => path,
    };
    format!("{stem}.glb")
}

pub struct ModelExporter;

impl Exporter for ModelExporter {
    fn name(&self) -> &'static str {
        "model-glb"
    }

    fn version(&self) -> u32 {
        1
    }

    fn kind(&self) -> Kind {
        Kind::Model
    }

    fn export(&self, input: &ExportInput) -> Result<Vec<Output>, ExportError> {
        let model = read(input)?;
        Ok(vec![Output { path: output_path(input.path), data: glb::write(&model, input.path) }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_paths() {
        assert_eq!(output_path("/A/B_skel.fbx"), "/A/B_skel.glb");
        assert_eq!(output_path("/A/B.FBX"), "/A/B.glb");
        assert_eq!(output_path("/A/PatchMesh"), "/A/PatchMesh.glb");
        assert_eq!(output_path("/A/Terrain.tmesh"), "/A/Terrain.tmesh.glb");
    }
}
