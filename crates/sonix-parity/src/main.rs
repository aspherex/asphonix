//! Image metrics for the parity runner.
//!
//! `parity-diff <web.png> <native.png> <out-dir> <text-rects.json> [dpr] [tol]`
//! writes `<out-dir>/diff.png` (red = outside text, blue = inside text, brightness = error) and
//! prints one JSON object:
//! * `nonText`: pixels outside the (2 px dilated) text mask whose max channel delta > `tol`
//! * `text`: inside the mask: differing pixels, mean luminance of both images, ink coverage
//!   (mean |lum - local bg|, bg = mask-region mode of each image) and their ratio.
use image::{Rgba, RgbaImage};
use std::path::Path;

fn lum(p: &Rgba<u8>) -> f64 {
    0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64
}

/// `parity-diff crop <web.png> <native.png> <out.png> <x> <y> <w> <h> [zoom]`: side-by-side (web over native) crop
/// in CSS px (dpr 1.5), nearest-neighbour zoomed, for eyeballing individual elements.
fn crop(a: &[String]) {
    let web = image::open(&a[2]).expect("web").to_rgba8();
    let nat = image::open(&a[3]).expect("native").to_rgba8();
    let f = |i: usize| a[i].parse::<f64>().unwrap();
    let (x, y, w, h) = ((f(5) * 1.5) as u32, (f(6) * 1.5) as u32, (f(7) * 1.5) as u32, (f(8) * 1.5) as u32);
    let z: u32 = a.get(9).and_then(|s| s.parse().ok()).unwrap_or(3);
    let mut out = RgbaImage::new(w * z, (h * 2 + 2) * z);
    for (row, img) in [&web, &nat].iter().enumerate() {
        for yy in 0..h {
            for xx in 0..w {
                if x + xx >= img.width() || y + yy >= img.height() {
                    continue;
                }
                let p = *img.get_pixel(x + xx, y + yy);
                for dy in 0..z {
                    for dx in 0..z {
                        out.put_pixel(xx * z + dx, ((row as u32) * (h + 2) + yy) * z + dy, p);
                    }
                }
            }
        }
    }
    out.save(&a[4]).unwrap();
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.get(1).map(String::as_str) == Some("crop") {
        return crop(&a);
    }
    if a.len() < 5 {
        eprintln!("usage: parity-diff web.png native.png out-dir text-rects.json [dpr] [tol]");
        std::process::exit(2);
    }
    let dpr: f64 = a.get(5).and_then(|s| s.parse().ok()).unwrap_or(1.5);
    let tol: i32 = a.get(6).and_then(|s| s.parse().ok()).unwrap_or(2);
    let web = image::open(&a[1]).expect("web png").to_rgba8();
    let nat = image::open(&a[2]).expect("native png").to_rgba8();
    let rects: Vec<[f64; 4]> = serde_json::from_str(&std::fs::read_to_string(&a[4]).unwrap_or("[]".into())).unwrap_or_default();
    if web.dimensions() != nat.dimensions() {
        println!("{}", serde_json::json!({"error": format!("size mismatch web {:?} native {:?}", web.dimensions(), nat.dimensions())}));
        return;
    }
    let (w, h) = web.dimensions();
    let mut mask = vec![false; (w * h) as usize];
    let dil = 2i64;
    for r in &rects {
        let x0 = ((r[0] * dpr).floor() as i64 - dil).max(0);
        let y0 = ((r[1] * dpr).floor() as i64 - dil).max(0);
        let x1 = (((r[0] + r[2]) * dpr).ceil() as i64 + dil).min(w as i64);
        let y1 = (((r[1] + r[3]) * dpr).ceil() as i64 + dil).min(h as i64);
        for y in y0..y1 {
            for x in x0..x1 {
                mask[(y as u64 * w as u64 + x as u64) as usize] = true;
            }
        }
    }
    let mut diff = RgbaImage::new(w, h);
    let (mut nt_bad, mut nt_tot, mut t_bad, mut t_tot) = (0u64, 0u64, 0u64, 0u64);
    let (mut lw, mut ln) = (0f64, 0f64);
    let mut worst_nt = 0i32;
    let mut hist = std::collections::HashMap::<(u8, u8), u64>::new();
    let mut hist_n = std::collections::HashMap::<(u8, u8), u64>::new();
    // bbox of non-text differences (device px) to guide the eye
    let (mut bx0, mut by0, mut bx1, mut by1) = (w, h, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            let pw = web.get_pixel(x, y);
            let pn = nat.get_pixel(x, y);
            let d = (0..3).map(|c| (pw[c] as i32 - pn[c] as i32).abs()).max().unwrap();
            let m = mask[(y * w + x) as usize];
            let v = (d * 4).min(255) as u8;
            diff.put_pixel(x, y, if m { Rgba([0, v / 2, v, 255]) } else { Rgba([v, 0, 0, 255]) });
            if m {
                t_tot += 1;
                if d > tol {
                    t_bad += 1;
                }
                lw += lum(pw);
                ln += lum(pn);
                *hist.entry((pw[0], pw[1])).or_default() += 1;
                *hist_n.entry((pn[0], pn[1])).or_default() += 1;
            } else {
                nt_tot += 1;
                if d > tol {
                    nt_bad += 1;
                    worst_nt = worst_nt.max(d);
                    bx0 = bx0.min(x);
                    by0 = by0.min(y);
                    bx1 = bx1.max(x);
                    by1 = by1.max(y);
                }
            }
        }
    }
    // ink coverage: mean |lum - bg| in the mask, bg = most common colour inside the mask
    let ink = |img: &RgbaImage, hist: &std::collections::HashMap<(u8, u8), u64>| -> f64 {
        let bg = hist.iter().max_by_key(|(_, c)| **c).map(|(k, _)| *k).unwrap_or((0, 0));
        let mut bgp = Rgba([bg.0, bg.1, 0, 255]);
        // find a pixel with that (r,g) to read its b
        'o: for y in 0..h {
            for x in 0..w {
                if mask[(y * w + x) as usize] {
                    let p = img.get_pixel(x, y);
                    if (p[0], p[1]) == bg {
                        bgp = *p;
                        break 'o;
                    }
                }
            }
        }
        let bl = lum(&bgp);
        let mut s = 0.0;
        for y in 0..h {
            for x in 0..w {
                if mask[(y * w + x) as usize] {
                    s += (lum(img.get_pixel(x, y)) - bl).abs();
                }
            }
        }
        s / 255.0
    };
    let (iw, inat) = (ink(&web, &hist), ink(&nat, &hist_n));
    std::fs::create_dir_all(&a[3]).ok();
    diff.save(Path::new(&a[3]).join("diff.png")).ok();
    let out = serde_json::json!({
        "size": [w, h],
        "tol": tol,
        "nonText": {"pixels": nt_tot, "differing": nt_bad, "fraction": nt_bad as f64 / nt_tot.max(1) as f64, "worstDelta": worst_nt,
                    "bboxDevicePx": if nt_bad > 0 { serde_json::json!([bx0, by0, bx1, by1]) } else { serde_json::Value::Null }},
        "text": {"pixels": t_tot, "differing": t_bad, "fraction": t_bad as f64 / t_tot.max(1) as f64,
                 "meanLumWeb": lw / t_tot.max(1) as f64, "meanLumNative": ln / t_tot.max(1) as f64,
                 "inkWeb": iw, "inkNative": inat, "inkRatioNativeOverWeb": if iw > 0.0 { inat / iw } else { 0.0 }},
    });
    println!("{out}");
}
