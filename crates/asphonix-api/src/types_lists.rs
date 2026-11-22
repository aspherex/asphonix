//! Wire types of the list routes (history, liked, smart playlists, settings extras).
//! Kept in its own file so the core `types.rs` stays small.
use crate::types::{FlowArc, Track};
use serde::{Deserialize, Serialize};

/// `LikedSummary`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LikedSummary {
    pub count: usize,
    pub duration: f64,
}

/// `HistoryItem`: a day header row or a played track (tagged by `kind`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HistoryItem {
    Day { day: String, count: u32 },
    #[serde(rename_all = "camelCase")]
    Track { track: Box<Track>, played_at: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SmartMatchMode {
    All,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SmartRuleField {
    Rating,
    Starred,
    PlayCount,
    LastPlayed,
    DateAdded,
    Genre,
    Year,
    Artist,
    Duration,
    Format,
    BitDepth,
    SampleRate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SmartRuleOp {
    Eq,
    Gte,
    Lte,
    Unrated,
    Yes,
    No,
    Never,
    OlderThanDays,
    WithinDays,
    Is,
    IsNot,
    Contains,
}

/// `SmartRule.value`: a number or a string (absent for value-less ops).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SmartValue {
    Num(f64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartRule {
    pub field: SmartRuleField,
    pub op: SmartRuleOp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<SmartValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SmartSort {
    Random,
    RecentlyAdded,
    MostPlayed,
    RecentlyPlayed,
    Title,
    Artist,
    Rating,
}

/// `SmartInput`: what the editor creates/edits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartInput {
    pub name: String,
    pub match_mode: SmartMatchMode,
    pub rules: Vec<SmartRule>,
    pub sort: SmartSort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// `SmartPlaylist` + the live result (`SmartSummary`: `trackCount`, `duration`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartSummary {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(flatten)]
    pub input: SmartInput,
    pub track_count: usize,
    pub duration: f64,
}

/// `SmartPreset`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartPreset {
    pub key: String,
    pub description: String,
    #[serde(flatten)]
    pub input: SmartInput,
}

/// `PlayPlaylistRequest` / `PlayLikedRequest` / `PlaySmartRequest` (shared shape).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaySourceRequest {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_track_id: Option<String>,
    #[serde(default)]
    pub seed: u32,
    /// Flow: play as a Flow source with this arc (overrides shuffle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<FlowArc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovePlaylistTracksRequest {
    pub id: String,
    pub indices: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderPlaylistRequest {
    pub id: String,
    pub track_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ReplayGainMode {
    #[default]
    Off,
    Track,
    Album,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum OverlayCorner {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// `McpStatus`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub enabled: bool,
    pub allow_control: bool,
    pub listening: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_path: Option<String>,
    pub bridge_exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub register_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// `ArtCacheStatus`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtCacheStatus {
    pub cached: u64,
    pub total: u64,
    pub bytes: u64,
}

/// One `settings_set_*` command; the backend applies it.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingChange {
    Scrobbling(bool),
    AutoCheckUpdates(bool),
    SpreadArtists(bool),
    QueueWindowSize(u32),
    Autoplay(bool),
    SyncPlayQueue(bool),
    ReplayGain(ReplayGainMode),
    PlayThreshold(u32),
    TrackOverlay(bool),
    TrackOverlayCorner(OverlayCorner),
    FreeMemoryWhenMinimized(bool),
    McpEnabled(bool),
    McpAllowControl(bool),
    VerboseLogging(bool),
}
