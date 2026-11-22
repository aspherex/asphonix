//! The ASPHONIX colour palette and fonts as plain Rust values: a dark theme and a light one ("Editorial Laboratory").
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
            clip: c(0xff3b30),
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
            clip: c(0xe11d48),
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
