//! The tuxDrop icon, drawn the same on every platform: a blue drop with an
//! arrow pointing down, and a dot while a request waits for an answer. The
//! tray icon, the notification icon and the package logos all come from it.

/// The drop's blue.
const BLUE: [u8; 3] = [0x25, 0x63, 0xEB];

/// The drop as `size` × `size` RGBA pixels, in blue.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn drop_rgba(size: u32, attention: bool) -> Vec<u8> {
    drop_rgba_in(BLUE, size, attention)
}

/// The drop as `size` × `size` RGBA pixels, painted in `color`.
pub fn drop_rgba_in(color: [u8; 3], size: u32, attention: bool) -> Vec<u8> {
    const SAMPLES: u32 = 4;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut covered = 0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = (px as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    if drop_pixel(x, y, attention) {
                        covered += 1;
                    }
                }
            }
            let alpha = (covered * 255 / (SAMPLES * SAMPLES)) as u8;
            rgba.extend_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }
    rgba
}

/// Whether the point (`x`, `y`) of the unit square is painted.
fn drop_pixel(x: f32, y: f32, attention: bool) -> bool {
    let inside_circle = |cx: f32, cy: f32, r: f32| (x - cx).powi(2) + (y - cy).powi(2) <= r * r;
    // Badge in the top right corner, with a gap around it.
    if attention {
        if inside_circle(0.80, 0.20, 0.15) {
            return true;
        }
        if inside_circle(0.80, 0.20, 0.23) {
            return false;
        }
    }
    // Drop: a circle and the two tangents from its tip.
    let (cx, cy, r, tip) = (0.5f32, 0.62f32, 0.32f32, 0.06f32);
    let d = cy - tip;
    let tangent_len = d * (1.0 - (r / d).powi(2));
    let half_width = (y - tip) * r / (d * d - r * r).sqrt();
    let in_drop = inside_circle(cx, cy, r)
        || (y >= tip && y <= tip + tangent_len && (x - cx).abs() <= half_width);
    // Arrow cut out of it.
    let stem = (x - cx).abs() <= 0.045 && (0.42..=0.70).contains(&y);
    let head = (0.62..=0.82).contains(&y) && (x - cx).abs() <= (0.82 - y) * 0.85;
    in_drop && !stem && !head
}
