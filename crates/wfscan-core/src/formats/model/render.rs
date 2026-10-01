//! A small software renderer: one shaded picture of a [`Model`], for previews and for
//! checking by eye that a model read correctly. No textures: flat-shaded triangles lit
//! from the camera, darkened by the baked ambient occlusion.

use super::model::Model;

/// Where the camera is: angles in radians around the model's centre, and a zoom factor
/// (1 fits the model in the picture).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
}

impl Default for View {
    fn default() -> Self {
        Self { yaw: 0.5, pitch: 0.2, zoom: 1.0 }
    }
}

/// Render `model` as `width` x `height` RGBA pixels, rows top to bottom, on a transparent
/// background.
pub fn render(model: &Model, width: u32, height: u32, view: &View) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut pixels = vec![0u8; w * h * 4];
    if model.positions.is_empty() || w == 0 || h == 0 {
        return pixels;
    }
    // A rigged model's skin mirrors it in X; show it as it will look skinned.
    let mirror = if model.skeleton.is_some() { -1.0 } else { 1.0 };
    let mut low = [f32::INFINITY; 3];
    let mut high = [f32::NEG_INFINITY; 3];
    for p in &model.positions {
        for axis in 0..3 {
            low[axis] = low[axis].min(p[axis]);
            high[axis] = high[axis].max(p[axis]);
        }
    }
    let centre = [0, 1, 2].map(|axis| (low[axis] + high[axis]) / 2.0);
    let radius = (0..3).map(|axis| (high[axis] - low[axis]).powi(2)).sum::<f32>().sqrt() / 2.0;
    let scale = w.min(h) as f32 * 0.47 * view.zoom / radius.max(f32::MIN_POSITIVE);
    let (sin_yaw, cos_yaw) = view.yaw.sin_cos();
    let (sin_pitch, cos_pitch) = view.pitch.sin_cos();
    // Model space to screen: x right, y down, and a depth that grows towards the camera.
    let project = |p: &[f32; 3]| {
        let (x, y, z) = ((p[0] - centre[0]) * mirror, p[1] - centre[1], p[2] - centre[2]);
        let (x, z) = (x * cos_yaw + z * sin_yaw, -x * sin_yaw + z * cos_yaw);
        let (y, z) = (y * cos_pitch - z * sin_pitch, y * sin_pitch + z * cos_pitch);
        [w as f32 / 2.0 + x * scale, h as f32 / 2.0 - y * scale, z * scale]
    };
    let projected: Vec<[f32; 3]> = model.positions.iter().map(project).collect();

    let mut depth = vec![f32::NEG_INFINITY; w * h];
    for mesh in &model.meshes {
        for triangle in mesh.indices.as_chunks::<3>().0 {
            let [a, b, c] = triangle.map(|i| projected[i as usize]);
            let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            if area == 0.0 || !area.is_finite() {
                continue;
            }
            // Lit from the camera, the same from both sides: how squarely the triangle
            // faces the screen.
            let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
            let normal = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            let length = normal.iter().map(|n| n * n).sum::<f32>().sqrt();
            let facing = if length > 0.0 { (normal[2] / length).abs() } else { 0.0 };
            let occlusion = triangle.map(|i| f32::from(model.occlusion.get(i as usize).copied().unwrap_or(255)) / 255.0);

            let clamp = |value: f32, max: usize| (value.max(0.0) as usize).min(max);
            let (x0, x1) = (clamp(a[0].min(b[0]).min(c[0]).floor(), w - 1), clamp(a[0].max(b[0]).max(c[0]).ceil(), w - 1));
            let (y0, y1) = (clamp(a[1].min(b[1]).min(c[1]).floor(), h - 1), clamp(a[1].max(b[1]).max(c[1]).ceil(), h - 1));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let wa = ((b[0] - px) * (c[1] - py) - (b[1] - py) * (c[0] - px)) / area;
                    let wb = ((c[0] - px) * (a[1] - py) - (c[1] - py) * (a[0] - px)) / area;
                    let wc = 1.0 - wa - wb;
                    if wa < 0.0 || wb < 0.0 || wc < 0.0 {
                        continue;
                    }
                    let z = wa * a[2] + wb * b[2] + wc * c[2];
                    if z <= depth[y * w + x] {
                        continue;
                    }
                    depth[y * w + x] = z;
                    let shade = (0.18 + 0.82 * facing) * (0.45 + 0.55 * (wa * occlusion[0] + wb * occlusion[1] + wc * occlusion[2]));
                    let pixel = &mut pixels[(y * w + x) * 4..][..4];
                    pixel[0] = (205.0 * shade) as u8;
                    pixel[1] = (212.0 * shade) as u8;
                    pixel[2] = (225.0 * shade) as u8;
                    pixel[3] = 255;
                }
            }
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::super::model::Mesh;
    use super::*;

    fn square() -> Model {
        Model {
            positions: vec![[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]],
            uv0: vec![[0.0; 2]; 4],
            uv1: vec![[0.0; 2]; 4],
            occlusion: vec![255; 4],
            joints: vec![],
            weights: vec![],
            meshes: vec![Mesh { name: "Square".into(), material: None, indices: vec![0, 1, 2, 0, 2, 3] }],
            skeleton: None,
            scale: [1.0; 3],
        }
    }

    fn alpha(pixels: &[u8], width: u32, x: u32, y: u32) -> u8 {
        pixels[((y * width + x) * 4 + 3) as usize]
    }

    #[test]
    fn a_square_seen_from_the_front() {
        let view = View { yaw: 0.0, pitch: 0.0, zoom: 1.0 };
        let pixels = render(&square(), 64, 64, &view);
        assert_eq!(pixels.len(), 64 * 64 * 4);
        assert_eq!(alpha(&pixels, 64, 32, 32), 255, "the middle is covered");
        assert_eq!(alpha(&pixels, 64, 1, 1), 0, "the corners are not: the model is fitted with a margin");
        let covered = pixels.as_chunks::<4>().0.iter().filter(|p| p[3] == 255).count();
        assert!((1500..2600).contains(&covered), "{covered} of 4096 pixels");
        let middle = &pixels[(32 * 64 + 32) * 4..][..3];
        assert_eq!(middle, [205, 212, 225], "facing the camera, fully lit");
        // Both triangles meet without a seam along the diagonal.
        assert!((20..44).all(|i| alpha(&pixels, 64, i, 63 - i) == 255));
    }

    #[test]
    fn turned_away_it_gets_darker_and_narrower() {
        let front = render(&square(), 64, 64, &View { yaw: 0.0, pitch: 0.0, zoom: 1.0 });
        let turned = render(&square(), 64, 64, &View { yaw: 1.2, pitch: 0.0, zoom: 1.0 });
        let count = |pixels: &[u8]| pixels.as_chunks::<4>().0.iter().filter(|p| p[3] == 255).count();
        assert!(count(&turned) < count(&front) / 2 && count(&turned) > 0);
        assert!(turned[(32 * 64 + 32) * 4] < 150);
    }

    #[test]
    fn nothing_to_draw() {
        let mut empty = square();
        empty.positions.clear();
        empty.meshes.clear();
        assert!(render(&empty, 8, 8, &View::default()).iter().all(|&b| b == 0));
        assert!(render(&square(), 0, 8, &View::default()).is_empty());
    }
}
