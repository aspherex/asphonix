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

/// `parity-diff textcrop <web.png> <native.png> <out.png> <x> <y> <w> <h> [zoom]`: crop (CSS px, dpr 1.5) of one string, written
/// as web / native / |diff| stacked, plus JSON with mean luminance, ink coverage (mean |lum - bg| over the crop, bg = the most
/// common colour) and the mean absolute difference. Used for the text-fidelity report.
fn textcrop(a: &[String]) {
    let web = image::open(&a[2]).expect("web").to_rgba8();
    let nat = image::open(&a[3]).expect("native").to_rgba8();
    let f = |i: usize| a[i].parse::<f64>().unwrap();
    let (x, y, w, h) = ((f(5) * 1.5).floor() as u32, (f(6) * 1.5).floor() as u32, (f(7) * 1.5).ceil() as u32, (f(8) * 1.5).ceil() as u32);
    let z: u32 = a.get(9).and_then(|s| s.parse().ok()).unwrap_or(6);
    let px = |img: &RgbaImage, xx: u32, yy: u32| *img.get_pixel((x + xx).min(img.width() - 1), (y + yy).min(img.height() - 1));
    let stats = |img: &RgbaImage| {
        let mut hist = std::collections::HashMap::<[u8; 3], u32>::new();
        for yy in 0..h {
            for xx in 0..w {
                let p = px(img, xx, yy);
                *hist.entry([p[0], p[1], p[2]]).or_default() += 1;
            }
        }
        let bg = hist.iter().max_by_key(|(_, c)| **c).map(|(k, _)| *k).unwrap();
        let bgl = lum(&Rgba([bg[0], bg[1], bg[2], 255]));
        let (mut sl, mut ink, mut max_dev) = (0.0, 0.0, 0.0f64);
        for yy in 0..h {
            for xx in 0..w {
                let l = lum(&px(img, xx, yy));
                sl += l;
                ink += (l - bgl).abs() / 255.0;
                max_dev = max_dev.max((l - bgl).abs());
            }
        }
        (sl / (w * h) as f64, ink, max_dev)
    };
    let (lw, iw, mw) = stats(&web);
    let (ln, inat, mn) = stats(&nat);
    let mut out = RgbaImage::new(w * z, (h * 3 + 4) * z);
    let mut sad = 0.0;
    for yy in 0..h {
        for xx in 0..w {
            let (pw, pn) = (px(&web, xx, yy), px(&nat, xx, yy));
            let d = (0..3).map(|c| (pw[c] as i32 - pn[c] as i32).abs()).max().unwrap();
            sad += d as f64;
            let dv = Rgba([(d * 3).min(255) as u8, (d * 3).min(255) as u8, (d * 3).min(255) as u8, 255]);
            for dy in 0..z {
                for dx in 0..z {
                    out.put_pixel(xx * z + dx, yy * z + dy, pw);
                    out.put_pixel(xx * z + dx, (h + 2 + yy) * z + dy, pn);
                    out.put_pixel(xx * z + dx, (2 * h + 4 + yy) * z + dy, dv);
                }
            }
        }
    }
    out.save(&a[4]).unwrap();
    // best integer device-pixel shift of the native crop that minimises the luminance difference to the web crop
    let mut best = (f64::MAX, 0i32, 0i32);
    for sy in -3..=3i32 {
        for sx in -3..=3i32 {
            let mut sad = 0.0;
            for yy in 3..h.saturating_sub(3) {
                for xx in 3..w.saturating_sub(3) {
                    let pw = px(&web, xx, yy);
                    let pn = px(&nat, (xx as i32 + sx) as u32, (yy as i32 + sy) as u32);
                    sad += (lum(&pw) - lum(&pn)).abs();
                }
            }
            if sad < best.0 {
                best = (sad, sx, sy);
            }
        }
    }
    println!("{}", serde_json::json!({"bestShiftDevPx": [best.1, best.2], "meanLumWeb": lw, "meanLumNative": ln, "inkWeb": iw, "inkNative": inat, "inkRatio": if iw > 0.0 { inat / iw } else { 0.0 }, "peakContrastWeb": mw, "peakContrastNative": mn, "meanAbsDiff": sad / (w * h) as f64}));
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.get(1).map(String::as_str) == Some("crop") {
        return crop(&a);
    }
    if a.get(1).map(String::as_str) == Some("textcrop") {
        return textcrop(&a);
    }
    if a.len() < 5 {
        eprintln!("usage: parity-diff web.png native.png out-dir text-rects.json [dpr] [tol]");
        std::process::exit(2);
    }
    let dpr: f64 = a.get(5).and_then(|s| s.parse().ok()).unwrap_or(1.5);
    let tol: i32 = a.get(6).and_then(|s| s.parse().ok()).unwrap_or(2);
    // optional 7th arg "x,y,w,h" (CSS px): extra stats for just that region (animation frames, spectrum)
    let region: Option<[f64; 4]> = a.get(7).and_then(|s| {
        let v: Vec<f64> = s.split(',').filter_map(|p| p.parse().ok()).collect();
        (v.len() == 4).then(|| [v[0], v[1], v[2], v[3]])
    });
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
    // optional 8th arg "x,y" (CSS px of an element's left edge, vertical middle): the border colour there in both images
    // (the pixel within 4 device px of the edge that differs most from the interior), for transition sampling
    let edge: Option<serde_json::Value> = a.get(8).and_then(|s| {
        let v: Vec<f64> = s.split(',').filter_map(|p| p.parse().ok()).collect();
        (v.len() == 2).then(|| {
            let (ex, ey) = ((v[0] * dpr).floor() as i64, (v[1] * dpr).round() as i64);
            let pick = |img: &RgbaImage| {
                let interior = *img.get_pixel((ex + 8) as u32, ey as u32);
                let mut best = (0i32, interior);
                for dx in -1..4i64 {
                    let p = *img.get_pixel((ex + dx).max(0) as u32, ey as u32);
                    let d = (0..3).map(|c| (p[c] as i32 - interior[c] as i32).abs()).sum::<i32>();
                    if d > best.0 {
                        best = (d, p);
                    }
                }
                [best.1[0], best.1[1], best.1[2]]
            };
            serde_json::json!({"web": pick(&web), "native": pick(&nat)})
        })
    });
    let region_stats = region.map(|r| {
        let (x0, y0) = ((r[0] * dpr).floor() as u32, (r[1] * dpr).floor() as u32);
        let (x1, y1) = ((((r[0] + r[2]) * dpr).ceil() as u32).min(w), (((r[1] + r[3]) * dpr).ceil() as u32).min(h));
        let (mut bad, mut tot, mut sum, mut worst) = (0u64, 0u64, 0f64, 0i32);
        for y in y0..y1 {
            for x in x0..x1 {
                let (pw, pn) = (web.get_pixel(x, y), nat.get_pixel(x, y));
                let d = (0..3).map(|c| (pw[c] as i32 - pn[c] as i32).abs()).max().unwrap();
                tot += 1;
                sum += d as f64;
                worst = worst.max(d);
                if d > tol {
                    bad += 1;
                }
            }
        }
        serde_json::json!({"pixels": tot, "differing": bad, "fraction": bad as f64 / tot.max(1) as f64, "meanDelta": sum / tot.max(1) as f64, "worstDelta": worst})
    });
    let out = serde_json::json!({
        "region": region_stats,
        "edge": edge,
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
