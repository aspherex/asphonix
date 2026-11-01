//! SONIX tokens (`src/styles/tokens.css` of the SoniX repo) as plain Rust values.
//! Dark = `:root`, light = `:root[data-theme="light"]` ("Editorial Laboratory").
//! Switching theme = swapping this `Copy` struct (no CSS variables needed).
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
    pub base: Hsla,    // --color-surface-base
    pub low: Hsla,     // --color-surface-low
    pub mid: Hsla,     // --color-surface-mid
    pub high: Hsla,    // --color-surface-high
    pub highest: Hsla, // --color-surface-highest
    pub accent: Hsla,
    pub accent_text: Hsla, // text on accent fills
    pub accent_fg: Hsla,   // accent used as text/line color (olive in light)
    pub accent_hover: Hsla,
    pub clip: Hsla,
    pub border: Hsla, // --color-border-idle
    pub frame: Hsla,  // --color-border-frame
    pub focus: Hsla,  // --color-border-focus
    pub text: Hsla,   // --color-text-stark
    pub muted: Hsla,  // --color-text-muted
    pub meter: Hsla,  // --color-meter (canvas bars)
    pub shadow: Hsla, // hard offset shadow colour
    pub chrome: Hsla, // --color-chrome (translucent rails)
    /// `--font-body`: Inter in dark, Space Grotesk in light.
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
