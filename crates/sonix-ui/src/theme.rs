//! The SONIX colour palette and fonts as plain Rust values: a dark theme and a light one ("Editorial Laboratory").
//! Switching theme = swapping this `Copy` struct.
use gpui::{Hsla, rgb, rgba};

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}
fn ca(hex: u32) -> Hsla {
    rgba(hex).into()
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub light: bool,
    // surfaces, from the page background up
    pub base: Hsla,
    pub low: Hsla,
    pub mid: Hsla,
    pub high: Hsla,
    pub highest: Hsla,
    pub accent: Hsla,
    pub accent_text: Hsla, // text on accent fills
    pub accent_fg: Hsla,   // accent used as text/line color (olive in light)
    pub accent_hover: Hsla,
    pub clip: Hsla,
    pub border: Hsla, // idle border / rules
    pub frame: Hsla,  // strong frame border
    pub focus: Hsla,  // focus / hover border
    pub text: Hsla,   // primary text
    pub muted: Hsla,  // secondary text
    pub meter: Hsla,  // spectrum / meter bars
    pub shadow: Hsla, // hard offset shadow colour
    pub chrome: Hsla, // translucent rails
    /// Body font: Inter in dark, Space Grotesk in light.
    pub body_font: &'static str,
}

pub const DISPLAY: &str = "Space Grotesk";
pub const SANS: &str = "Inter";
pub const MONO: &str = "JetBrains Mono";

impl Theme {
    pub fn dark() -> Self {
        Self {
            light: false,
            base: c(0x0a0a0c),
            low: c(0x131418),
            mid: c(0x1a1c22),
            high: c(0x242731),
            highest: c(0x272a34),
            accent: c(0xd4ff00),
            accent_text: c(0x0a0a0c),
            accent_fg: c(0xd4ff00),
            accent_hover: c(0xffffff),
            clip: c(0xff6b61), // >= 5.1:1 as text on every dark surface (WCAG 1.4.3)
            border: c(0x272a34),
            frame: c(0x272a34),
            focus: c(0xd4ff00),
            text: c(0xffffff),
            muted: c(0x8e92a0),
            meter: c(0xd4ff00),
            shadow: c(0x000000),
            chrome: ca(0x131418b8),
            body_font: SANS,
        }
    }

    pub fn light() -> Self {
        Self {
            light: true,
            base: c(0xf5f5f7),
            low: c(0xffffff),
            mid: c(0xebebef),
            high: c(0xe2e2e6),
            highest: c(0xd9d9de),
            accent: c(0xd4ff00),
            accent_text: c(0x0e0e10),
            accent_fg: c(0x536600),
            accent_hover: c(0xeaff66),
            clip: c(0xb0103a), // >= 5.0:1 as text on every light surface (WCAG 1.4.3)
            border: c(0xe2e2e6),
            frame: c(0x1a1a1d),
            focus: c(0x0e0e10),
            text: c(0x0e0e10),
            muted: c(0x5f5e66),
            meter: c(0x0e0e10),
            shadow: c(0x1a1a1d),
            chrome: ca(0xffffffad),
            body_font: DISPLAY,
        }
    }

    pub fn toggled(&self) -> Self {
        if self.light { Self::dark() } else { Self::light() }
    }
}

#[cfg(test)]
mod contrast_tests {
    //! WCAG 2.2 contrast guard (docs/ACCESSIBILITY.md): text tokens need 4.5:1 on every surface they can sit on,
    //! focus indicators 3:1 against the page. Fails when a palette change drops below the thresholds.
    use super::Theme;
    use gpui::Hsla;

    fn luminance(c: Hsla) -> f32 {
        let rgb = c.to_rgb();
        let lin = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        0.2126 * lin(rgb.r) + 0.7152 * lin(rgb.g) + 0.0722 * lin(rgb.b)
    }

    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    fn check(th: &Theme, name: &str) {
        let surfaces = [("base", th.base), ("low", th.low), ("mid", th.mid), ("high", th.high), ("highest", th.highest)];
        let texts = [("text", th.text), ("muted", th.muted), ("accent_fg", th.accent_fg), ("clip", th.clip)];
        for (sn, s) in surfaces {
            for (tn, t) in texts {
                let r = contrast(t, s);
                assert!(r >= 4.5, "{name}: `{tn}` on `{sn}` is {r:.2}:1, needs 4.5:1 (WCAG 1.4.3)");
            }
        }
        let r = contrast(th.accent_text, th.accent);
        assert!(r >= 4.5, "{name}: `accent_text` on `accent` is {r:.2}:1, needs 4.5:1");
        let r = contrast(th.focus, th.base);
        assert!(r >= 3.0, "{name}: `focus` vs `base` is {r:.2}:1, needs 3:1 (WCAG 1.4.11)");
    }

    #[test]
    fn dark_theme_meets_wcag_contrast() {
        check(&Theme::dark(), "dark");
    }

    #[test]
    fn light_theme_meets_wcag_contrast() {
        check(&Theme::light(), "light");
    }
}
