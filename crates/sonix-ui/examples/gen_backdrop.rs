//! Pre-renders the app's canvas background gradients (GPUI cannot draw radial
//! gradients) into two small dithered PNGs under `assets/bg/`. The image is stretched
//! over the shell once; 480x270 keeps the decoded texture at ~0.5 MB while the soft
//! washes stay smooth. Run: `cargo run -p sonix-ui --example gen_backdrop`.
//! Premultiplied compositing: layers are listed top-first, so they are painted in
//! reverse over the base.
use image::{Rgb, RgbImage};

const W: u32 = 480;
const H: u32 = 270;

struct Radial {
    rx: f32,
    ry: f32,
    cx: f32,
    cy: f32,
    color: [f32; 3],
    alpha: f32,
    stop: f32,
}

fn over(dst: [f32; 3], src: [f32; 3], a: f32) -> [f32; 3] {
    [dst[0] * (1.0 - a) + src[0] * a, dst[1] * (1.0 - a) + src[1] * a, dst[2] * (1.0 - a) + src[2] * a]
}

fn render(base: impl Fn(f32, f32) -> [f32; 3], layers_top_first: &[Radial], path: &str) {
    let mut img = RgbImage::new(W, H);
    for y in 0..H {
        for x in 0..W {
            let (u, v) = ((x as f32 + 0.5) / W as f32, (y as f32 + 0.5) / H as f32);
            let mut c = base(u, v);
            for l in layers_top_first.iter().rev() {
                let t = (((u - l.cx) / l.rx).powi(2) + ((v - l.cy) / l.ry).powi(2)).sqrt();
                let a = l.alpha * (1.0 - t / l.stop).clamp(0.0, 1.0);
                c = over(c, l.color, a);
            }
            // ordered dither (+-0.5 LSB) against banding
            let d = (((x * 7 + y * 13) % 16) as f32 / 16.0) - 0.5;
            let px: Vec<u8> = c.iter().map(|ch| (ch * 255.0 + d).round().clamp(0.0, 255.0) as u8).collect();
            img.put_pixel(x, y, Rgb([px[0], px[1], px[2]]));
        }
    }
    img.save(path).expect("write png");
    println!("wrote {path}");
}

fn rgb(r: u8, g: u8, b: u8) -> [f32; 3] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}

fn main() {
    let r = |rx, ry, cx, cy, c: [f32; 3], alpha| Radial { rx, ry, cx, cy, color: c, alpha, stop: 0.62 };
    // dark: "oil slick" over #0a0a0c
    render(
        |_, _| rgb(10, 10, 12),
        &[
            r(0.90, 0.70, 0.0, 0.0, rgb(104, 58, 180), 0.32),
            r(0.80, 0.70, 1.0, 0.0, rgb(40, 84, 176), 0.28),
            r(0.90, 0.80, 1.0, 1.0, rgb(16, 132, 128), 0.26),
            r(0.90, 0.80, 0.0, 1.0, rgb(132, 44, 112), 0.22),
        ],
        "assets/bg/dark.png",
    );
    // light: pearl. base = 135° diagonal linear gradient #f7f4f8 -> #f2f5f9
    let (a, b) = (rgb(0xf7, 0xf4, 0xf8), rgb(0xf2, 0xf5, 0xf9));
    let base = move |u: f32, v: f32| {
        let (w, h) = (W as f32, H as f32);
        let len = (w + h) * std::f32::consts::FRAC_1_SQRT_2;
        let t = (((u - 0.5) * w + (v - 0.5) * h) * std::f32::consts::FRAC_1_SQRT_2 / len + 0.5).clamp(0.0, 1.0);
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
    };
    render(
        base,
        &[
            Radial { rx: 0.55, ry: 0.45, cx: 0.5, cy: 0.45, color: rgb(252, 241, 222), alpha: 0.5, stop: 0.70 },
            r(0.90, 0.70, 0.0, 0.0, rgb(250, 222, 236), 0.85),
            r(0.80, 0.70, 1.0, 0.0, rgb(216, 232, 252), 0.85),
            r(0.90, 0.80, 1.0, 1.0, rgb(216, 244, 232), 0.85),
            r(0.90, 0.80, 0.0, 1.0, rgb(232, 222, 250), 0.85),
        ],
        "assets/bg/light.png",
    );
}
