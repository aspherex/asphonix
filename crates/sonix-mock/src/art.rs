//! Procedural covers for the mock (deterministic per album/artist number): hard-edged
//! geometric sleeves in a few palettes, generated at the exact requested size.
use crate::library::Rng;
use sonix_api::ArtPixels;

fn hsv(h: f64, s: f64, v: f64) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let (c, i) = (v * s, h.floor() as u32);
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match i {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8]
}

pub fn procedural_cover(n: usize, artist: bool, size: u32) -> ArtPixels {
    let mut r = Rng::for_seed(n as u64 * 5 + if artist { 99 } else { 1 });
    let hue = r.next() * 360.0;
    let bg = hsv(hue, 0.55, 0.16);
    let fg = hsv(hue + 20.0, 0.75, 0.95);
    let alt = hsv(hue + 170.0, 0.6, 0.7);
    let pattern = n % 4;
    let (cx, cy) = (0.3 + r.next() * 0.4, 0.3 + r.next() * 0.4);
    let rad = 0.18 + r.next() * 0.22;
    let freq = 3.0 + (r.next() * 6.0).floor();
    let sz = size.max(1) as usize;
    let mut rgba = vec![255u8; sz * sz * 4];
    for y in 0..sz {
        let v = y as f64 / sz as f64;
        for x in 0..sz {
            let u = x as f64 / sz as f64;
            let col = match pattern {
                0 => {
                    let d = ((u - cx).powi(2) + (v - cy).powi(2)).sqrt();
                    if ((d * freq * 2.5).floor() as i64) % 2 == 0 { fg } else { bg }
                }
                1 => {
                    if (((u + v) * freq).floor() as i64) % 2 == 0 { bg } else if v > 0.66 { alt } else { fg }
                }
                2 => {
                    let (a, b) = ((u * freq).floor() as i64, (v * freq).floor() as i64);
                    if (a + b) % 2 == 0 { bg } else if (a * 7 + b * 3) % 5 == 0 { fg } else { alt }
                }
                _ => {
                    let d = ((u - cx).powi(2) + (v - cy).powi(2)).sqrt();
                    if d < rad { fg } else if v > 0.78 && u < 0.7 { alt } else { bg }
                }
            };
            let o = (y * sz + x) * 4;
            rgba[o..o + 3].copy_from_slice(&col);
        }
    }
    ArtPixels { width: size, height: size, rgba }
}
