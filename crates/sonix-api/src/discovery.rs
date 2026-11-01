//! Wire types of the library-browsing extras (`src/lib/ipc/types.ts`: Genre, ArtistInfo, ArtistTopTracks,
//! HistoryEntry) and the `player_play_genre` / `player_play_artist` requests. Kept in their own file so the
//! shared `types.rs` stays untouched by the library-routes port.
use crate::types::{Artist, Track};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Genre {
    pub name: String,
    pub album_count: u32,
    pub track_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GenreSort {
    Tracks,
    Alphabetical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetGenresRequest {
    pub offset: usize,
    pub limit: usize,
    pub sort: GenreSort,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayGenreRequest {
    pub name: String,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_track_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayArtistRequest {
    pub id: String,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_track_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TopSource {
    Popular,
    MostPlayed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistTopTracks {
    pub source: TopSource,
    pub items: Vec<Track>,
    pub total: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub biography: Option<String>,
    pub similar: Vec<Artist>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub track: Track,
    pub played_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecentRequest {
    pub limit: usize,
}

/// `session_check_server_queue`: the play queue another client saved on the server (never applied automatically).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerQueueOffer {
    pub track: Track,
    pub position_sec: f64,
    pub track_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<String>,
}
