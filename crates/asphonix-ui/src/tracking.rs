//! The type scale: family, size, line height, letter-spacing (tracking), weight and
//! caps for every text style. GPUI 0.2.x has no letter-spacing in `TextStyle`; the
//! tracking is applied by `text::Txt`, which paints each glyph at `x + i * tracking`.
use crate::theme::{DISPLAY, MONO};

/// One entry of the type scale.
#[derive(Clone, Copy, Debug)]
pub struct TypeSpec {
    pub family: Family,
    pub size: f32,
    pub line: f32,
    /// Letter-spacing in em, added after every glyph.
    pub tracking_em: f32,
    pub weight: f32,
    /// Rendered in uppercase.
    pub caps: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Display,
    Body,
    Mono,
}

impl Family {
    pub fn name(self, body: &'static str) -> &'static str {
        match self {
            Family::Display => DISPLAY,
            Family::Body => body,
            Family::Mono => MONO,
        }
    }
}

macro_rules! ty {
    ($n:ident, $fam:ident, $size:expr, $line:expr, $tr:expr, $w:expr, $caps:expr) => {
        // the whole scale is kept, including steps no screen uses yet
        #[allow(dead_code)]
        pub const $n: TypeSpec = TypeSpec { family: Family::$fam, size: $size, line: $line, tracking_em: $tr, weight: $w, caps: $caps };
    };
}
// name, family, size, line, tracking (em), weight, caps
ty!(DISPLAY_XL, Display, 48.0, 52.0, -0.03, 700.0, true);
ty!(HEADLINE_LG, Display, 32.0, 38.0, -0.02, 700.0, true);
ty!(HEADLINE_MD, Display, 24.0, 30.0, -0.01, 600.0, true);
ty!(HEADLINE_SM, Display, 18.0, 24.0, 0.0, 600.0, true);
ty!(BODY_LG, Body, 15.0, 22.0, -0.01, 500.0, false);
ty!(BODY_MD, Body, 13.0, 18.0, 0.0, 400.0, false);
ty!(BODY_SM, Body, 12.0, 16.0, 0.01, 400.0, false);
ty!(MONO_METRIC, Mono, 20.0, 24.0, -0.02, 700.0, false);
ty!(MONO_DATA, Mono, 12.0, 16.0, 0.02, 500.0, false);
ty!(MONO_CODE, Mono, 11.0, 14.0, 0.04, 400.0, false);
ty!(LABEL_CAPS, Mono, 10.0, 12.0, 0.08, 700.0, true);

impl TypeSpec {
    pub fn tracking_px(&self) -> f32 {
        self.tracking_em * self.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_px_scales_with_size() {
        assert!((LABEL_CAPS.tracking_px() - 0.8).abs() < 1e-6, "0.08em of 10px");
        assert!((DISPLAY_XL.tracking_px() + 1.44).abs() < 1e-5, "-0.03em of 48px");
    }

    #[test]
    fn type_scale_values() {
        assert_eq!((LABEL_CAPS.size, LABEL_CAPS.line, LABEL_CAPS.weight), (10.0, 12.0, 700.0));
        assert_eq!((LABEL_CAPS.caps, HEADLINE_MD.caps, MONO_DATA.caps), (true, true, false));
        assert_eq!((BODY_MD.size, BODY_MD.line), (13.0, 18.0));
        assert_eq!(MONO_DATA.family, Family::Mono);
    }
}
