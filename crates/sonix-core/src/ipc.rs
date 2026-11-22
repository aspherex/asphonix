//! Wire types shared by the core modules: library rows, queue/player state,
//! output status, settings, events.
//!
//! Every struct/enum here uses `#[serde(rename_all = "camelCase")]`.
//! Optional fields are serialized with
//! `#[serde(skip_serializing_if = "Option::is_none")]`, so an absent value is
//! omitted rather than sent as `null`; the one exception is
//! `PlayerState.track`, which is always present (`null` when idle).

// Not every type/field is constructed or read by every consumer.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};




// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcErrorCode {
    NotConnected,
    Network,
    AuthFailed,
    NotFound,
    ServerError,
    Io,
    Decode,
    OutputUnavailable,
    Unimplemented,
    /// The request itself is not allowed in the current state (e.g. removing
    /// the active profile).
    InvalidRequest,
    Unknown,
}

#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct IpcError {
    pub code: IpcErrorCode,
    pub message: String,
}

impl IpcError {
    pub fn unimplemented(what: &str) -> Self {
        Self {
            code: IpcErrorCode::Unimplemented,
            message: format!("{what} is not implemented yet"),
        }
    }
}

pub type IpcResult<T> = Result<T, IpcError>;

// ---------------------------------------------------------------------------
// Common
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Paged<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageRequest {
    pub offset: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReplayGain {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_gain_db: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_gain_db: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_peak: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_peak: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackFormat {
    pub suffix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    pub sampling_rate: u32,
    pub channel_count: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    pub duration: f64,
    pub format: TrackFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    /// 1-5 user rating (`rating_set`), absent = unrated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    /// File size in bytes, when the server reports it (Subsonic `size`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_gain: Option<ReplayGain>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// ISO timestamp the file was added to the server library, if the
    /// server exposes it (`RawSong.created`) — backs the all-tracks list's
    /// "date added" sort. Never guessed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_added: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    pub name: String,
    pub artist: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    pub song_count: u32,
    pub duration: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub play_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub album_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub public: bool,
    pub song_count: u32,
    pub duration: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    // --- smart-lists block ---
    /// Server-side rule-based (Navidrome `.nsp`) playlist: the server flags it
    /// read-only although the current user owns it. Contents refresh server-side.
    pub smart: bool,
    /// The current user can't edit tracks/name/delete (smart, or another
    /// user's playlist). The UI disables every mutation for these.
    pub read_only: bool,
    /// The raw OpenSubsonic `readonly` flag as stored (`None` = the server
    /// didn't send one). Not on the wire: `smart`/`read_only` are derived
    /// from it by `library::playlist_access_flags`.
    #[serde(skip)]
    pub server_read_only: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumDetail {
    pub album: Album,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistDetail {
    pub artist: Artist,
    pub albums: Vec<Album>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDetail {
    pub playlist: Playlist,
    pub tracks: Vec<Track>,
}

// ---------------------------------------------------------------------------
// Auth / session
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectRequest {
    pub url: String,
    pub username: String,
    pub password: String,
}

/// A saved account (`profiles_list`). Passwords never leave the keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    /// Stable id: `secrets::profiles::profile_id(url, username)`.
    pub id: String,
    pub url: String,
    pub username: String,
    /// The account in `connection.json` (the one the app is connected to).
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileIdRequest {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    pub server_name: String,
    pub server_version: String,
    pub api_version: String,
    pub open_subsonic_extensions: Vec<String>,
    pub ping_ms: u32,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    pub state: ConnectionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ping_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_count: Option<u64>,
}

// ---------------------------------------------------------------------------
// Library sync
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SyncPhase {
    Artists,
    Albums,
    Tracks,
    Playlists,
    Done,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgressEvent {
    pub phase: SyncPhase,
    pub current: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStartRequest {
    pub full: bool,
}

// ---------------------------------------------------------------------------
// Browse
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ArtistSort {
    Alphabetical,
    Random,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetArtistsRequest {
    pub offset: u64,
    pub limit: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<ArtistSort>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AlbumSort {
    Alphabetical,
    Artist,
    Newest,
    Recent,
    Frequent,
    Random,
    Year,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AlbumFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starred_only: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAlbumsRequest {
    pub offset: u64,
    pub limit: u64,
    pub sort: AlbumSort,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<AlbumFilter>,
}

// ---------------------------------------------------------------------------
// Browse: all tracks (`browse_get_tracks`) — the Tracks route.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TrackSortField {
    Title,
    Artist,
    Album,
    Year,
    DateAdded,
    Duration,
    Format,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    fn sql(self) -> &'static str {
        match self {
            SortDir::Asc => "ASC",
            SortDir::Desc => "DESC",
        }
    }
}

/// Builds the `ORDER BY` clause for every all-tracks sort — shared by
/// `queries::get_tracks` (paged, for the Tracks route) and
/// `queries::get_all_track_ids` (`player_play_all`'s base order before an
/// optional real shuffle). Each field has its own index (`schema.rs`'s
/// `idx_tracks_*`) so a page never costs a full scan/sort — see
/// `library::perf_tests`.
///
/// `qualified`: the FTS-filtered variant of both queries joins `tracks_fts`
/// (which has its own `title`/`artist`/`album` columns) against `tracks t`,
/// so its `ORDER BY` must reference `t.<col>` — unqualified names there are
/// ambiguous for any query shape that doesn't happen to re-expose the same
/// name as an output column (`t.*`'s column aliases mask it; a narrower
/// column list like `t.id, t.artist` does not). The unfiltered path queries
/// `tracks` directly with no alias, so it uses the bare column name (and
/// matches the index definitions verbatim).
///
/// Every tie-break column uses the *same* direction as the primary one
/// (`{d}` on both), not a direction fixed to ASC: each `idx_tracks_*` index
/// is created plain ascending on every column, and an ascending index can
/// only satisfy an `ORDER BY` without a temp-sort when the requested
/// per-column directions are either all forward (ASC, ASC - a plain scan) or
/// all reversed (DESC, DESC - the exact same scan backward). Mixing
/// directions (e.g. `year DESC, title ASC`) matches neither and forces
/// SQLite into "USE TEMP B-TREE FOR RIGHT PART OF ORDER BY", which is what a
/// benchmark against 50k rows caught (~180ms instead of <1ms for a deep
/// page) before this was fixed. The trade-off — ties sort Z→A too when the
/// primary direction is flipped — is a common, acceptable convention (same
/// as most spreadsheet/file-manager sort toggles) in exchange for both
/// directions of every field staying index-only.
pub(crate) fn track_order_by_sql(sort: TrackSortField, dir: SortDir, qualified: bool) -> String {
    let d = dir.sql();
    let p = if qualified { "t." } else { "" };
    match sort {
        TrackSortField::Title => format!("{p}title COLLATE NOCASE {d}"),
        TrackSortField::Artist => format!("{p}artist COLLATE NOCASE {d}, {p}title COLLATE NOCASE {d}"),
        // No COALESCE on the tie-break columns: SQLite can only use an index
        // to satisfy ORDER BY when the clause matches the indexed columns
        // verbatim (`idx_tracks_album_title`) — wrapping them would force a
        // temp B-tree sort over the whole table. Nulls sort first in ASC
        // (last in DESC), an acceptable tie-break since the primary sort is
        // the album name.
        TrackSortField::Album => format!("{p}album COLLATE NOCASE {d}, {p}disc_number {d}, {p}track_number {d}"),
        TrackSortField::Year => format!("{p}year {d}, {p}title COLLATE NOCASE {d}"),
        TrackSortField::DateAdded => format!("{p}created {d}, {p}title COLLATE NOCASE {d}"),
        TrackSortField::Duration => format!("{p}duration {d}"),
        TrackSortField::Format => format!("{p}suffix COLLATE NOCASE {d}, {p}title COLLATE NOCASE {d}"),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTracksRequest {
    pub offset: u64,
    pub limit: u64,
    pub sort: TrackSortField,
    pub dir: SortDir,
    /// Local FTS/LIKE filter on title/artist/album (debounced client-side).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeSections {
    pub recently_played: Vec<Album>,
    pub frequent: Vec<Album>,
    pub newest: Vec<Album>,
    pub random: Vec<Album>,
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SearchType {
    All,
    Track,
    Album,
    Artist,
    Playlist,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub offset: u64,
    pub limit: u64,
    pub text: String,
    pub search_type: SearchType,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub tracks: Paged<Track>,
    pub albums: Paged<Album>,
    pub artists: Paged<Artist>,
    pub playlists: Paged<Playlist>,
}

// ---------------------------------------------------------------------------
// Playlists CRUD
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePlaylistRequest {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamePlaylistRequest {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletePlaylistRequest {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddToPlaylistRequest {
    pub id: String,
    pub track_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveFromPlaylistRequest {
    pub id: String,
    pub indices: Vec<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderPlaylistRequest {
    pub id: String,
    pub track_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicatePlaylistRequest {
    pub id: String,
}

// ---------------------------------------------------------------------------
// Star / unstar
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StarTargetType {
    Track,
    Album,
    Artist,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StarRequest {
    pub id: String,
    pub star_type: StarTargetType,
}

// ---------------------------------------------------------------------------
// Rating (ratings reuse `StarTargetType` for `kind` — same three target
// kinds, no need for a second identical enum)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRatingRequest {
    pub id: String,
    pub kind: StarTargetType,
    /// 1-5, or 0 to clear the rating.
    pub rating: u8,
}

// ---------------------------------------------------------------------------
// Radio ("start radio" seeded from a track/album/artist)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioSeed {
    pub kind: StarTargetType,
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioStartRequest {
    pub seed: RadioSeed,
}

// ---------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayQueueRequest {
    pub track_ids: Vec<String>,
    pub start_index: u32,
}

/// `player_play_all`'s order: either a real shuffle of the whole (filtered)
/// library, or the Tracks route's current sort/direction. Resolved to an id
/// list from local SQLite, so the UI never
/// builds or sends a 50k-id array.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TrackOrder {
    Shuffle,
    Sort { field: TrackSortField, dir: SortDir },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayAllRequest {
    pub order: TrackOrder,
    /// Same local FTS/LIKE filter as `browse_get_tracks` — "shuffle/play
    /// all" plays exactly the current filtered view, not the whole library.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Row to start from (e.g. double-click/Enter on a specific track in
    /// `order: sort` mode); defaults to the first track in the resolved
    /// order when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_track_id: Option<String>,
    /// feat-flow: play as a Flow source with this arc (overrides `shuffle`).
    #[serde(default)]
    pub flow: Option<FlowArc>,
}

/// `playlist_detail`/`playlists_get` already fetch every track for the
/// detail page's own virtualized list, but play/shuffle resolve the id order
/// fresh from local SQLite here (`library::get_playlist_play_source`) rather
/// than reuse that already-fetched `Vec<Track>` — the whole point of the
/// rolling window is that playing a 1,980-track playlist never has to build
/// (or shuffle) a 1,980-element structure for the *queue*; only this id (+
/// artist name) list does, and it's the same shape `player_play_all` already
/// uses for "all tracks".
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayPlaylistRequest {
    pub id: String,
    pub shuffle: bool,
    /// Row to start from (defaults to the first track / a random one when
    /// shuffling, same convention as `PlayAllRequest.start_track_id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_track_id: Option<String>,
    /// feat-flow: play as a Flow source with this arc (overrides `shuffle`).
    #[serde(default)]
    pub flow: Option<FlowArc>,
}

/// Same shape as `PlayPlaylistRequest`, for an artist's "Play all"/"Shuffle".
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayArtistRequest {
    pub id: String,
    pub shuffle: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_track_id: Option<String>,
    /// feat-flow: play as a Flow source with this arc (overrides `shuffle`).
    #[serde(default)]
    pub flow: Option<FlowArc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeekRequest {
    pub position_sec: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSpectrumActiveRequest {
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetVolumeRequest {
    pub volume: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetShuffleRequest {
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RepeatMode {
    Off,
    All,
    One,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRepeatRequest {
    pub mode: RepeatMode,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderQueueRequest {
    pub from_index: u32,
    pub to_index: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveFromQueueRequest {
    pub index: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JumpToRequest {
    /// Play-order position, same indexing as `player_get_queue`'s items.
    pub index: u32,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EnqueuePosition {
    Next,
    End,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnqueueRequest {
    pub track_ids: Vec<String>,
    pub position: EnqueuePosition,
}

/// What a rolling-window `Source` (`audio::queue::Queue`) actually is. Drives
/// the queue rail's "NEXT FROM <label>" section header; `Album`/`Search` are
/// defined for completeness with the full model but have no producing
/// command yet (album play stays a plain static `player_play_queue`, and
/// there's no "search play all" in the UI).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    Playlist,
    AllTracks,
    Artist,
    Album,
    Radio,
    Search,
    /// Genre "Play/Shuffle" (`discovery::player_play_genre`).
    Genre,
    // smart-lists block: starred tracks ("Liked Songs") and local smart playlists.
    Liked,
    Smart,
}

/// A source's draw mode: a real, OS-entropy-seeded Fisher-Yates permutation
/// with no repeats until the whole pool has been drawn once, or the pool's
/// own stored order. Mirrors `Queue::shuffle` 1:1 (`shuffle == true` iff
/// `mode == Shuffle`) — exposed separately here only because `QueueState`
/// reports it alongside the source, not because it can diverge.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SourceMode {
    Shuffle,
    InOrder,
    /// feat-flow: DJ-style sequencing (`audio::sequence`), per source and
    /// independent of the global shuffle flag. The arc is `QueueSourceInfo.arc`.
    Flow,
}

/// Energy arc of a Flow source (`audio::sequence`): `steady` (no arc term),
/// `build` (rising) or `peak` (rise, then cool down), over a ~40-track span.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum FlowArc {
    #[default]
    Steady,
    Build,
    Peak,
}

/// Why a queue row sits where it does, real values only: every field is
/// omitted when unknown (`QueueItem.flow` itself is omitted when empty).
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FlowReason {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bpm_from: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bpm_to: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_to: Option<String>,
    /// Set (to the genre name) only when both tracks have the same genre.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_genre: Option<bool>,
}

/// How much of a Flow source's pool has real sequencing data (counts).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FlowCoverage {
    pub total: u32,
    pub bpm: u32,
    pub key: u32,
    pub loudness: u32,
}

/// Real-count summary of the active rolling-window source (`audio::queue`'s
/// module doc comment), for the queue rail's "NEXT FROM <label> · SHUFFLE"
/// section and its "+ 1,960 more from <label>" footer. `remaining` is
/// exactly how many pool ids haven't been drawn into the window yet — never
/// an estimate, and never the window's own (already-shown) contents.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSourceInfo {
    pub kind: SourceKind,
    pub label: String,
    /// The playlist/artist id behind `label` (playlist/artist sources only),
    /// so "Playing from <label>" can link to it without a name lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub mode: SourceMode,
    pub remaining: u64,
    /// Flow sources only: the energy arc and the pool's real data coverage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arc: Option<FlowArc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_coverage: Option<FlowCoverage>,
}

/// Whether a queue entry is something the user explicitly queued ("Play
/// next"/"Add to queue") or was auto-drawn from the active `Source` — the
/// queue rail's "NEXT IN QUEUE" vs. "NEXT FROM <source>" split.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum QueueItemOrigin {
    User,
    Source,
}

/// A single `QueueState.items` row: a `Track` plus where it came from.
/// `#[serde(flatten)]` puts `origin` alongside the track's own fields on the
/// wire (not nested), so `QueueState.items` stays a track-shaped array with
/// one extra field, not an array of `{track, origin}` wrapper objects.
#[derive(Debug, Clone, Serialize)]
pub struct QueueItem {
    #[serde(flatten)]
    pub track: Track,
    pub origin: QueueItemOrigin,
    /// Flow sources: why this row follows the previous one (real values only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow: Option<FlowReason>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetQueueRequest {
    pub offset: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueState {
    /// Just the requested page (`GetQueueRequest.offset`/`limit`) — the
    /// engine resolves metadata lazily for only these ids, never the whole
    /// queue (see `audio::engine`'s module doc comment). With a rolling
    /// `source` active, `total` itself also never reflects the source's
    /// full pool (see `audio::queue`'s module doc comment) — only `source`
    /// below does, via `remaining`.
    pub items: Vec<QueueItem>,
    pub total: u64,
    pub current_index: i64,
    /// The active rolling-window source, if any — entirely the engine's own
    /// queue model now (`Queue::source_info`), not a best-effort UI label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<QueueSourceInfo>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
    Loading,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OutputMode {
    Exclusive,
    Shared,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerState {
    pub status: PlaybackStatus,
    pub track: Option<Track>,
    pub position_sec: f64,
    pub duration_sec: f64,
    pub output_mode: OutputMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    /// True when the device stream runs at the 32-bit float mixer format
    /// (WASAPI shared mode); `bit_depth` is then the mixer's, not the source's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub float_mix: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_label: Option<String>,
    /// Volume slider position 0..1 (what `player_set_volume` sets). The
    /// engine maps it to gain with a dB taper (`audio::output::volume_to_gain`).
    pub volume: f32,
    /// The attenuation actually applied, in dB (0.0 at the top, -60 at the
    /// bottom of the taper); absent = muted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_db: Option<f32>,
    /// Engine-owned truth: true only while output is bit-identical to the
    /// decoded source (exclusive, native rate/depth, no resampling or
    /// fallback format, unity volume, no ReplayGain/DSP). The UI never
    /// derives this itself.
    pub bit_perfect: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub queue_length: u32,
    /// Most recent playback failure (stream stalled/errored, device open
    /// failed, ...), if any — the engine already skips to the next track on
    /// its own; this is purely informational so the UI can show what
    /// happened. `None` once a track is actually playing again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// ReplayGain currently applied to the audible track, in dB (after the
    /// peak clamp). Present only while "Volume normalization" is on, the
    /// track carries a tag and the gain actually changes the signal; while
    /// present, `bit_perfect` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_gain_db: Option<f32>,
    /// A-B loop markers (seconds into the audible track): `loop_a` once A is
    /// set, both once the loop is active. Cleared on track change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_a: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_b: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpectrumFrame {
    pub bands: Vec<u8>,
    /// Left/right peak level in dBFS (`-90.0` = silence, `0.0` = full scale)
    /// of the samples sent to the device since the previous frame, measured
    /// after the volume stage. A mono source reports the same value twice.
    pub peak_l: f32,
    pub peak_r: f32,
}

// ---------------------------------------------------------------------------
// Output devices
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetOutputDeviceRequest {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetOutputModeRequest {
    pub mode: OutputMode,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub mode: OutputMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_label: Option<String>,
    /// Same engine-authoritative meaning as `PlayerState.bit_perfect`.
    pub bit_perfect: bool,
    /// Why exclusive mode is not in use although it was requested (device
    /// rejected every native-rate format, device held by another app,
    /// exclusive control disallowed in Windows sound settings, device lost),
    /// as a named WASAPI HRESULT. Omitted when exclusive is active or shared
    /// mode was chosen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclusive_error: Option<String>,
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub scrobbling_enabled: bool,
    /// "Spread artists in shuffle" (default off): post-processes a real
    /// shuffle so the same artist rarely plays back-to-back. Mirrored onto
    /// the engine's `Queue::spread_artists` on every toggle and at startup.
    pub spread_artists_in_shuffle: bool,
    /// "Queue window" (`audio::queue`'s rolling window): how many auto
    /// (source-drawn) tracks are materialized in the queue at once, 10-100,
    /// default 20. Mirrors `Queue::window_size`.
    pub queue_window_size: u32,
    // --- continuity ---
    /// "Autoplay similar music when the queue ends" (default on).
    pub autoplay_enabled: bool,
    /// "Sync play queue with server" (`savePlayQueue`/`getPlayQueue`, default on).
    pub sync_play_queue: bool,
    /// "Volume normalization (ReplayGain)": off (default) / track / album.
    pub replay_gain_mode: crate::audio::replaygain::ReplayGainMode,
    /// "Check for updates automatically" (default on): the background
    /// 10 s / 6 h update check in `updates::`. Never installs by itself.
    pub auto_check_updates: bool,
    /// "Count a play after" (10-300 s, default 30): seconds of real listening
    /// after which a play enters the listening history and is scrobbled.
    pub play_threshold_sec: u32,
    /// "Show track preview" (default on): the always-on-top corner card on a
    /// track change while SONIX isn't focused.
    pub track_overlay: bool,
    /// "Free memory when minimized" (default on): release the main window's UI
    /// resources after 30 s minimized and rebuild them on restore.
    pub free_memory_when_minimized: bool,
    /// Corner of the work area for that card (default top left).
    pub track_overlay_corner: crate::overlay::OverlayCorner,
    /// "Claude (MCP)" (default off): the local bridge for `sonix-mcp` (`mcp_bridge::`).
    pub mcp_enabled: bool,
    /// "Allow playback control" for MCP clients (default off).
    pub mcp_allow_control: bool,
    /// "Verbose logging" (default off): debug-level events in the log file.
    pub verbose_logging: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetScrobblingEnabledRequest {
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSpreadArtistsInShuffleRequest {
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetQueueWindowSizeRequest {
    pub size: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPlayThresholdRequest {
    pub seconds: u32,
}

// ---------------------------------------------------------------------------
// Art cache status (background prefetch progress, `art::prefetch`)
// ---------------------------------------------------------------------------

/// Real counters only — `cached`/`total` come from the same id list and
/// file-exists checks `art::prefetch::Prefetcher` uses to decide what to
/// fetch, `bytes` from the cache's own on-disk accounting. Before the first
/// prefetch pass of a session has run, `total` is `0` (not a guess).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtCacheStatus {
    pub cached: u32,
    pub total: u32,
    pub bytes: u64,
}

// ---------------------------------------------------------------------------
// Event names
// ---------------------------------------------------------------------------

pub const EVENT_PLAYER_STATE: &str = "player-state";
pub const EVENT_SPECTRUM_FRAME: &str = "spectrum-frame";
pub const EVENT_SYNC_PROGRESS: &str = "sync-progress";
pub const EVENT_CONNECTION_STATUS: &str = "connection-status";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStatusEvent {
    pub state: ConnectionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ping_ms: Option<u32>,
}
