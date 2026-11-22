//! Setting value types and ranges shared by the settings store (`secrets`) and the modules that use them.
//!
//! This module is a leaf: it depends on nothing else in the crate, so `secrets` (persistence) can describe the
//! settings without depending on `audio`, `scrobble` or `overlay`, and those modules re-export what they own.

use serde::{Deserialize, Serialize};

/// Default `queueWindowSize` (`Settings`), before any user override.
pub const DEFAULT_WINDOW_SIZE: usize = 20;
pub const MIN_WINDOW_SIZE: usize = 10;
pub const MAX_WINDOW_SIZE: usize = 100;

/// "Count a play after" setting range and default, in seconds.
pub const MIN_PLAY_THRESHOLD_SEC: u32 = 10;
pub const MAX_PLAY_THRESHOLD_SEC: u32 = 300;
pub const DEFAULT_PLAY_THRESHOLD_SEC: u32 = 30;

/// "Volume normalization (ReplayGain)" mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplayGainMode {
    #[default]
    Off,
    Track,
    Album,
}

impl ReplayGainMode {
    pub fn to_u8(self) -> u8 {
        match self {
            ReplayGainMode::Off => 0,
            ReplayGainMode::Track => 1,
            ReplayGainMode::Album => 2,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => ReplayGainMode::Track,
            2 => ReplayGainMode::Album,
            _ => ReplayGainMode::Off,
        }
    }
}

/// Which corner of the work area the track-change card sits in.
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

/// Source label prefix of autoplay ("radio") continuations; `listen` uses it to tell autoplay plays apart.
pub const AUTOPLAY_LABEL_PREFIX: &str = "Autoplay · ";
