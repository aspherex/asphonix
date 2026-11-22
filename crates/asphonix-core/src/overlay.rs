use serde::{Deserialize, Serialize};

/// Which corner of the work area the card sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OverlayCorner {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl OverlayCorner {
    pub fn to_u8(self) -> u8 {
        match self {
            OverlayCorner::TopLeft => 0,
            OverlayCorner::TopRight => 1,
            OverlayCorner::BottomLeft => 2,
            OverlayCorner::BottomRight => 3,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => OverlayCorner::TopRight,
            2 => OverlayCorner::BottomLeft,
            3 => OverlayCorner::BottomRight,
            _ => OverlayCorner::TopLeft,
        }
    }

}
