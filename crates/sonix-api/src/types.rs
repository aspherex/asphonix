//! Wire types. Mirrors `src/lib/ipc/types.ts` and `src-tauri/src/ipc.rs`.
//! Every struct is `camelCase` on the wire; enums carry the exact attribute the
//! Rust side of SoniX uses (documented per type where it is not camelCase).
use serde::{Deserialize, Serialize};

fn is_false(b: &bool) -> bool {
    !*b
}

// --------------------------------------------------------------- common
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paged<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub offset: usize,
}

impl<T> Paged<T> {
    pub fn empty() -> Self {
        Self { items: Vec::new(), total: 0, offset: 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageRequest {
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayGain {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_gain_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_gain_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_peak: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_peak: Option<f64>,
}

/// Real, measured format data for one file. Never fabricated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackFormat {
    pub suffix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    pub sampling_rate: u32,
    pub channel_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    pub duration: f64,
    pub format: TrackFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    /// ISO timestamp when starred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_gain: Option<ReplayGain>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_added: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueItemOrigin {
    User,
    Source,
}

/// A `Track` flattened on the wire plus where it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    #[serde(flatten)]
    pub track: Track,
    pub origin: QueueItemOrigin,
    /// Flow sources: why this row follows the previous one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<FlowReason>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    pub name: String,
    pub artist: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    pub song_count: u32,
    pub duration: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    pub id: String,
    pub name: String,
    pub album_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starred: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub public: bool,
    pub song_count: u32,
    pub duration: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_art: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub smart: bool,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumDetail {
    pub album: Album,
    pub tracks: Vec<Track>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistDetail {
    pub artist: Artist,
    pub albums: Vec<Album>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistDetail {
    pub playlist: Playlist,
    pub tracks: Vec<Track>,
}

// --------------------------------------------------------------- auth/session
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectRequest {
    pub url: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub url: String,
    pub username: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    pub server_name: String,
    pub server_version: String,
    pub api_version: String,
    pub open_subsonic_extensions: Vec<String>,
    pub ping_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    pub state: ConnectionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ping_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_count: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncPhase {
    Artists,
    Albums,
    Tracks,
    Playlists,
    Done,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgressEvent {
    pub phase: SyncPhase,
    pub current: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStartRequest {
    pub full: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStatusEvent {
    pub state: ConnectionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ping_ms: Option<u32>,
}

// --------------------------------------------------------------- browse
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTracksRequest {
    pub offset: usize,
    pub limit: usize,
    pub sort: TrackSortField,
    pub dir: SortDir,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtistSort {
    Alphabetical,
    Random,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetArtistsRequest {
    pub offset: usize,
    pub limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<ArtistSort>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub starred_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAlbumsRequest {
    pub offset: usize,
    pub limit: usize,
    pub sort: AlbumSort,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<AlbumFilter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HomeSections {
    pub recently_played: Vec<Album>,
    pub frequent: Vec<Album>,
    pub newest: Vec<Album>,
    pub random: Vec<Album>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchType {
    All,
    Track,
    Album,
    Artist,
    Playlist,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub offset: usize,
    pub limit: usize,
    pub text: String,
    pub search_type: SearchType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub tracks: Paged<Track>,
    pub albums: Paged<Album>,
    pub artists: Paged<Artist>,
    pub playlists: Paged<Playlist>,
}

// --------------------------------------------------------------- playlists / stars
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePlaylistRequest {
    pub name: String,
    #[serde(default)]
    pub track_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamePlaylistRequest {
    pub id: String,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletePlaylistRequest {
    pub id: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddToPlaylistRequest {
    pub id: String,
    pub track_ids: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StarTargetType {
    Track,
    Album,
    Artist,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StarRequest {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: StarTargetType,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRatingRequest {
    pub id: String,
    pub kind: StarTargetType,
    /// 0 clears, 1-5 sets.
    pub rating: u8,
}

// --------------------------------------------------------------- player
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayQueueRequest {
    pub track_ids: Vec<String>,
    pub start_index: usize,
}

/// Internally tagged: `{"kind":"shuffle"}` / `{"kind":"sort","field":..,"dir":..}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TrackOrder {
    Shuffle,
    Sort { field: TrackSortField, dir: SortDir },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayAllRequest {
    pub order: TrackOrder,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_track_id: Option<String>,
    /// Flow: play as a Flow source with this arc (overrides shuffle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<FlowArc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeekRequest {
    pub position_sec: f64,
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetVolumeRequest {
    /// 0.0 - 1.0 slider position (dB taper in the engine).
    pub volume: f64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RepeatMode {
    Off,
    All,
    One,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnqueuePosition {
    Next,
    End,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnqueueRequest {
    pub track_ids: Vec<String>,
    pub position: EnqueuePosition,
}

/// camelCase (`allTracks`, `inOrder`), like the Rust side of SoniX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    Playlist,
    AllTracks,
    Artist,
    Album,
    Radio,
    Search,
    Genre,
    Liked,
    Smart,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceMode {
    Shuffle,
    InOrder,
    Flow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSourceInfo {
    pub kind: SourceKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub mode: SourceMode,
    /// Exactly how many pool ids were not drawn into the window yet.
    pub remaining: usize,
    /// Flow sources only: the energy arc and the pool's real data coverage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arc: Option<FlowArc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_coverage: Option<FlowCoverage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueState {
    pub items: Vec<QueueItem>,
    pub total: usize,
    pub current_index: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<QueueSourceInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
    Loading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputMode {
    Exclusive,
    Shared,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerState {
    pub status: PlaybackStatus,
    pub track: Option<Track>,
    pub position_sec: f64,
    pub duration_sec: f64,
    pub output_mode: OutputMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub float_mix: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_label: Option<String>,
    pub volume: f64,
    /// Absent = muted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_db: Option<f64>,
    /// Engine-owned truth; the UI never derives it.
    pub bit_perfect: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub queue_length: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_gain_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_a: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_b: Option<f64>,
}

impl PlayerState {
    pub fn idle() -> Self {
        Self {
            status: PlaybackStatus::Stopped,
            track: None,
            position_sec: 0.0,
            duration_sec: 0.0,
            output_mode: OutputMode::Exclusive,
            sample_rate: None,
            bit_depth: None,
            float_mix: None,
            format_label: None,
            volume: 1.0,
            volume_db: Some(0.0),
            bit_perfect: false,
            shuffle: false,
            repeat: RepeatMode::Off,
            queue_length: 0,
            error: None,
            replay_gain_db: None,
            loop_a: None,
            loop_b: None,
        }
    }
}

/// 28 bands 0-255 plus L/R peak in dBFS (-90 = silence).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpectrumFrame {
    pub bands: Vec<u8>,
    pub peak_l: f32,
    pub peak_r: f32,
}

pub const SPECTRUM_BANDS: usize = 28;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Waveform {
    pub track_id: String,
    pub buckets: Vec<u8>,
    pub filled: usize,
    pub done: bool,
}

pub const WAVEFORM_BUCKETS: usize = 240;

// --------------------------------------------------------------- output / settings
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    pub mode: OutputMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_label: Option<String>,
    pub bit_perfect: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub scrobbling_enabled: bool,
    pub auto_check_updates: bool,
    pub spread_artists_in_shuffle: bool,
    pub queue_window_size: u32,
    pub autoplay_enabled: bool,
    pub sync_play_queue: bool,
    pub play_threshold_sec: u32,
    pub free_memory_when_minimized: bool,
    #[serde(default)]
    pub replay_gain_mode: crate::types_lists::ReplayGainMode,
    #[serde(default)]
    pub track_overlay: bool,
    #[serde(default)]
    pub track_overlay_corner: crate::types_lists::OverlayCorner,
    #[serde(default)]
    pub mcp_enabled: bool,
    #[serde(default)]
    pub mcp_allow_control: bool,
}

// --------------------------------------------------------------- art
/// Decoded cover pixels at an exact size (the native replacement for the
/// `sonix://art/{id}?s={size}` protocol): tightly packed RGBA8, `width*height*4` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Standard display sizes the backend may cache (mirrors `ART_SIZES`).
pub const ART_SIZES: [u32; 5] = [64, 160, 240, 320, 640];

/// Smallest allowed bucket that is >= `px` (the largest if none is).
pub fn bucket_for(px: u32) -> u32 {
    ART_SIZES.iter().copied().find(|s| *s >= px).unwrap_or(ART_SIZES[ART_SIZES.len() - 1])
}

// --------------------------------------------------------------- events
/// Everything the backend pushes (`IPC_EVENTS`).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    PlayerState(Box<PlayerState>),
    Spectrum(SpectrumFrame),
    SyncProgress(SyncProgressEvent),
    Connection(ConnectionStatusEvent),
    /// The queue changed (rows, order, source); re-fetch `player_get_queue`.
    QueueChanged,
    /// `update-available`: a newer release was found.
    UpdateAvailable(UpdateInfo),
    /// `update-progress` while `updates_install` downloads.
    UpdateProgress(UpdateProgress),
    /// Stars/ratings/playlists changed server side or by another view.
    LibraryChanged,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use serde_json::json;

    fn sample_track() -> Track {
        Track {
            id: "tr_1".into(),
            title: "T".into(),
            artist: "A".into(),
            artist_id: Some("ar_1".into()),
            album: None,
            album_id: None,
            album_artist: None,
            track_number: Some(2),
            disc_number: None,
            year: Some(2001),
            genre: None,
            duration: 12.5,
            format: TrackFormat { suffix: "flac".into(), bit_rate: None, bit_depth: Some(24), sampling_rate: 96000, channel_count: 2 },
            cover_art: Some("al_1".into()),
            starred: None,
            rating: None,
            size: None,
            replay_gain: None,
            path: None,
            date_added: Some("2026-01-01T00:00:00.000Z".into()),
        }
    }

    #[test]
    fn track_is_camel_case_and_skips_none() {
        let v = serde_json::to_value(sample_track()).unwrap();
        assert_eq!(v["artistId"], "ar_1");
        assert_eq!(v["trackNumber"], 2);
        assert_eq!(v["format"]["samplingRate"], 96000);
        assert_eq!(v["format"]["bitDepth"], 24);
        assert!(v.get("album").is_none() && v.get("rating").is_none() && v["format"].get("bitRate").is_none());
        let back: Track = serde_json::from_value(v).unwrap();
        assert_eq!(back, sample_track());
    }

    #[test]
    fn queue_item_flattens_track() {
        let it = QueueItem { track: sample_track(), origin: QueueItemOrigin::User, flow: None };
        let v = serde_json::to_value(&it).unwrap();
        assert_eq!(v["id"], "tr_1");
        assert_eq!(v["origin"], "user");
        assert!(v.get("track").is_none());
        let back: QueueItem = serde_json::from_value(v).unwrap();
        assert_eq!(back, it);
    }

    #[test]
    fn enums_use_their_own_cases() {
        assert_eq!(serde_json::to_value(ErrorCode::NotConnected).unwrap(), json!("not_connected"));
        assert_eq!(serde_json::to_value(SourceKind::AllTracks).unwrap(), json!("allTracks"));
        assert_eq!(serde_json::to_value(SourceMode::InOrder).unwrap(), json!("inOrder"));
        assert_eq!(serde_json::to_value(TrackSortField::DateAdded).unwrap(), json!("dateAdded"));
        assert_eq!(serde_json::to_value(PlaybackStatus::Loading).unwrap(), json!("loading"));
    }

    #[test]
    fn track_order_is_internally_tagged() {
        let s = serde_json::to_value(TrackOrder::Shuffle).unwrap();
        assert_eq!(s, json!({"kind": "shuffle"}));
        let o = TrackOrder::Sort { field: TrackSortField::Title, dir: SortDir::Desc };
        let v = serde_json::to_value(&o).unwrap();
        assert_eq!(v, json!({"kind": "sort", "field": "title", "dir": "desc"}));
        assert_eq!(serde_json::from_value::<TrackOrder>(v).unwrap(), o);
    }

    #[test]
    fn requests_and_paged_roundtrip() {
        let req = GetTracksRequest { offset: 100, limit: 100, sort: TrackSortField::Artist, dir: SortDir::Asc, filter: None };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v, json!({"offset":100,"limit":100,"sort":"artist","dir":"asc"}));
        let p = Paged { items: vec![1u8, 2], total: 9, offset: 3 };
        assert_eq!(serde_json::to_value(&p).unwrap(), json!({"items":[1,2],"total":9,"offset":3}));
        let star = StarRequest { id: "x".into(), kind: StarTargetType::Track };
        assert_eq!(serde_json::to_value(&star).unwrap(), json!({"id":"x","type":"track"}));
    }

    #[test]
    fn player_state_roundtrip_and_art_bucket() {
        let s = PlayerState::idle();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["positionSec"], 0.0);
        assert_eq!(v["bitPerfect"], false);
        assert!(v.get("track").unwrap().is_null());
        assert_eq!(serde_json::from_value::<PlayerState>(v).unwrap(), s);
        assert_eq!(bucket_for(28), 64);
        assert_eq!(bucket_for(65), 160);
        assert_eq!(bucket_for(9999), 640);
    }
}

// --------------------------------------------------------------- lyrics
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricLine {
    /// Milliseconds from the start of the track; present on every line of synced lyrics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u64>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LyricsSource {
    Opensubsonic,
    Classic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lyrics {
    pub synced: bool,
    pub lines: Vec<LyricLine>,
    pub source: LyricsSource,
}

// --------------------------------------------------------------- flow + radio
/// Energy arc of a Flow source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlowArc {
    Steady,
    Build,
    Peak,
}

/// Counts of tracks (distinct) with real sequencing data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowCoverage {
    pub total: u32,
    pub bpm: u32,
    pub key: u32,
    pub loudness: u32,
}

/// Why a queue row follows the previous one (Flow sources). Real values only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowReason {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm_from: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm_to: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_genre: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlowPoolKind {
    Playlist,
    Artist,
    Genre,
    Liked,
    Smart,
    AllTracks,
}

/// `flow_get_coverage`: the pool a Flow play of this source would use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowCoverageRequest {
    pub kind: FlowPoolKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RadioSeedKind {
    Track,
    Album,
    Artist,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioSeed {
    pub kind: RadioSeedKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioStartRequest {
    pub seed: RadioSeed,
}

// --------------------------------------------------------------- updater
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub available: bool,
    pub current_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Release notes, PLAIN TEXT - never render as markup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

/// `update-progress` event while `updates_install` downloads.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgress {
    pub downloaded: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}
