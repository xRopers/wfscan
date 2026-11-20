//! Radiance `.hdr` (RGBE) files, for BC6H textures: PNG would clamp them to 0-1.
//!
//! The format is the public Radiance picture format: a text header ending in a blank line,
//! the resolution line `-Y height +X width`, then one scanline after another. Each pixel
//! is a shared exponent and three mantissas (`r g b e`, value = m / 256 * 2^(e - 128)).
//! Scanlines are written run-length encoded the "new" way (`2 2 hi lo`, then each
//! channel as literal chunks of up to 128 bytes) where that is allowed (widths 8 to
//! 32767), flat otherwise, as readers expect.

/// One pixel as RGBE.
pub fn rgbe([r, g, b]: [f32; 3]) -> [u8; 4] {
    let [r, g, b] = [r, g, b].map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 });
    let largest = r.max(g).max(b);
    if largest < 1e-32 {
        return [0; 4];
    }
    // largest = mantissa * 2^exponent with mantissa in [0.5, 1).
    let exponent = largest.log2().floor() as i32 + 1;
    let scale = 256.0 / 2f32.powi(exponent);
    let channel = |v: f32| (v * scale).min(255.0) as u8;
    [channel(r), channel(g), channel(b), (exponent + 128).clamp(0, 255) as u8]
}

/// A whole `.hdr` file from `width` x `height` RGB floats, rows top to bottom.
pub fn write(width: u32, height: u32, pixels: &[[f32; 3]]) -> Vec<u8> {
    assert_eq!(pixels.len(), width as usize * height as usize, "one value per pixel");
    let mut out = format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n").into_bytes();
    let rle = (8..=0x7fff).contains(&width);
    for row in pixels.chunks(width.max(1) as usize) {
        let row: Vec<[u8; 4]> = row.iter().map(|&p| rgbe(p)).collect();
        if !rle {
            out.extend(row.iter().flatten());
            continue;
        }
        out.extend_from_slice(&[2, 2, (width >> 8) as u8, width as u8]);
        for channel in 0..4 {
            let values: Vec<u8> = row.iter().map(|p| p[channel]).collect();
            for chunk in values.chunks(128) {
                out.push(chunk.len() as u8);
                out.extend_from_slice(chunk);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read a file back (both scanline kinds), as a check of the writer.
    #[allow(clippy::needless_range_loop)]
    fn read(file: &[u8]) -> (u32, u32, Vec<[f32; 3]>) {
        let text_end = file.windows(2).position(|w| w == b"\n\n").unwrap() + 2;
        let line_end = text_end + file[text_end..].iter().position(|&b| b == b'\n').unwrap();
        let resolution = std::str::from_utf8(&file[text_end..line_end]).unwrap();
        let parts: Vec<&str> = resolution.split(' ').collect();
        let (height, width): (u32, u32) = (parts[1].parse().unwrap(), parts[3].parse().unwrap());
        let mut at = line_end + 1;
        let mut pixels = Vec::new();
        for _ in 0..height {
            let mut row = vec![[0u8; 4]; width as usize];
            if file[at..].starts_with(&[2, 2]) && (8..=0x7fff).contains(&width) {
                at += 4;
                for channel in 0..4 {
                    let mut x = 0;
                    while x < width as usize {
                        let n = file[at] as usize;
                        assert!(n <= 128, "only literal chunks are written");
                        for i in 0..n {
                            row[x + i][channel] = file[at + 1 + i];
                        }
                        at += 1 + n;
                        x += n;
                    }
                }
            } else {
                for p in &mut row {
                    p.copy_from_slice(&file[at..at + 4]);
                    at += 4;
                }
            }
            pixels.extend(row.iter().map(|&[r, g, b, e]| {
                if e == 0 { [0.0; 3] } else { [r, g, b].map(|m| (f32::from(m) + 0.5) / 256.0 * 2f32.powi(i32::from(e) - 128)) }
            }));
        }
        assert_eq!(at, file.len(), "no bytes left over");
        (width, height, pixels)
    }

    #[test]
    fn values_survive_within_rgbe_precision() {
        for width in [3u32, 8, 130] {
            let pixels: Vec<[f32; 3]> = (0..width * 2).map(|i| [i as f32 * 0.37, 1.0, 50.0 - i as f32 * 0.1]).collect();
            let (w, h, back) = read(&write(width, 2, &pixels));
            assert_eq!((w, h), (width, 2));
            for (a, b) in pixels.iter().zip(&back) {
                let largest = a[0].max(a[1]).max(a[2]);
                for c in 0..3 {
                    assert!((a[c] - b[c]).abs() <= largest / 128.0 + 1e-6, "{a:?} came back as {b:?}");
                }
            }
        }
        assert_eq!(rgbe([0.0, 0.0, 0.0]), [0; 4]);
        assert_eq!(rgbe([f32::NAN, -1.0, 0.0]), [0; 4]);
        assert_eq!(rgbe([1.0, 0.5, 0.0]), [128, 64, 0, 129]);
    }
}
