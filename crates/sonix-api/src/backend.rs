//! The `Backend` trait: `src/lib/ipc/commands.ts` as Rust.
//!
//! Design rules:
//! * Methods are synchronous and `Send + Sync`: the UI calls them from GPUI's
//!   background executor, never from the render path. A tokio-based real backend
//!   can `block_on` inside (it owns its runtime).
//! * One sub-trait per `commands.ts` namespace. Methods the first UI phases do not
//!   need have a default body returning [`ApiError::unimplemented`], so a backend
//!   (and a later phase) can implement command by command without touching the UI.
//! * Request/response types are the wire types of `types.rs`; the Rust command name
//!   from `ipc.rs` is in each doc comment so the mapping stays greppable.
use crate::discovery::*;
use crate::error::{ApiError, ApiResult};
use crate::types::*;
use crate::types_lists::*;
use std::sync::mpsc::Receiver;

macro_rules! unimpl {
    ($name:literal) => {
        Err(ApiError::unimplemented($name))
    };
}

/// `auth_*` (+ `profiles_*`).
pub trait AuthApi {
    /// `auth_connect`
    fn auth_connect(&self, _req: ConnectRequest) -> ApiResult<ServerCapabilities> {
        unimpl!("auth_connect")
    }
    /// `auth_probe`
    fn auth_probe(&self) -> ApiResult<ServerCapabilities> {
        unimpl!("auth_probe")
    }
    /// `auth_logout`
    fn auth_logout(&self) -> ApiResult<()> {
        unimpl!("auth_logout")
    }
    /// `auth_session_status`
    fn auth_session_status(&self) -> ApiResult<SessionStatus>;
    /// `profiles_list`
    fn profiles_list(&self) -> ApiResult<Vec<Profile>> {
        unimpl!("profiles_list")
    }
    /// `profiles_switch`
    fn profiles_switch(&self, _id: &str) -> ApiResult<ServerCapabilities> {
        unimpl!("profiles_switch")
    }
    /// `profiles_remove`
    fn profiles_remove(&self, _id: &str) -> ApiResult<()> {
        unimpl!("profiles_remove")
    }
}

/// `library_*`
pub trait LibraryApi {
    /// `library_sync_start` (progress arrives as `Event::SyncProgress`).
    fn library_sync_start(&self, _req: SyncStartRequest) -> ApiResult<()> {
        unimpl!("library_sync_start")
    }
    /// `browse_get_liked_summary`.count: number of liked (starred) tracks (sidebar "Liked Songs").
    fn liked_count(&self) -> ApiResult<u32> {
        unimpl!("liked_count")
    }
    /// `browse_get_liked` (newest like first)
    fn liked_get(&self, _req: PageRequest) -> ApiResult<Paged<Track>> {
        unimpl!("liked_get")
    }
    /// `browse_get_liked_summary`
    fn liked_summary(&self) -> ApiResult<LikedSummary> {
        unimpl!("liked_summary")
    }
    /// `history_get` (day-grouped flat list)
    fn history_get(&self, _req: PageRequest) -> ApiResult<Paged<HistoryItem>> {
        unimpl!("history_get")
    }
    /// `history_clear`
    fn history_clear(&self) -> ApiResult<()> {
        unimpl!("history_clear")
    }
    /// `smart_playlists_list`
    fn smart_list(&self) -> ApiResult<Vec<SmartSummary>> {
        unimpl!("smart_list")
    }
    /// `smart_playlists_get`
    fn smart_get(&self, _id: &str, _seed: u32) -> ApiResult<SmartSummary> {
        unimpl!("smart_get")
    }
    /// `smart_playlists_create`
    fn smart_create(&self, _input: SmartInput) -> ApiResult<SmartSummary> {
        unimpl!("smart_create")
    }
    /// `smart_playlists_update`
    fn smart_update(&self, _id: &str, _input: SmartInput) -> ApiResult<SmartSummary> {
        unimpl!("smart_update")
    }
    /// `smart_playlists_delete`
    fn smart_delete(&self, _id: &str) -> ApiResult<()> {
        unimpl!("smart_delete")
    }
    /// `smart_playlists_duplicate`
    fn smart_duplicate(&self, _id: &str) -> ApiResult<SmartSummary> {
        unimpl!("smart_duplicate")
    }
    /// `smart_playlists_presets`
    fn smart_presets(&self) -> ApiResult<Vec<SmartPreset>> {
        unimpl!("smart_presets")
    }
    /// `smart_playlists_tracks`
    fn smart_tracks(&self, _id: &str, _seed: u32, _req: PageRequest) -> ApiResult<Paged<Track>> {
        unimpl!("smart_tracks")
    }
    /// `smart_playlists_preview`
    fn smart_preview(&self, _input: SmartInput, _seed: u32, _req: PageRequest) -> ApiResult<Paged<Track>> {
        unimpl!("smart_preview")
    }
}

/// `browse_*`
pub trait BrowseApi {
    /// `browse_get_tracks`
    fn browse_get_tracks(&self, req: GetTracksRequest) -> ApiResult<Paged<Track>>;
    /// `browse_get_artists`
    fn browse_get_artists(&self, _req: GetArtistsRequest) -> ApiResult<Paged<Artist>> {
        unimpl!("browse_get_artists")
    }
    /// `browse_get_albums`
    fn browse_get_albums(&self, _req: GetAlbumsRequest) -> ApiResult<Paged<Album>> {
        unimpl!("browse_get_albums")
    }
    /// `browse_get_album`
    fn browse_get_album(&self, _id: &str) -> ApiResult<AlbumDetail> {
        unimpl!("browse_get_album")
    }
    /// `browse_get_artist`
    fn browse_get_artist(&self, _id: &str) -> ApiResult<ArtistDetail> {
        unimpl!("browse_get_artist")
    }
    /// `browse_get_artist_tracks`
    fn browse_get_artist_tracks(&self, _id: &str) -> ApiResult<Vec<Track>> {
        unimpl!("browse_get_artist_tracks")
    }
    /// `browse_get_home_sections`
    fn browse_get_home_sections(&self) -> ApiResult<HomeSections> {
        unimpl!("browse_get_home_sections")
    }
    // --- library routes port: genres, artist extras, recent history (types in `discovery.rs`)
    /// `browse_get_genres`
    fn browse_get_genres(&self, _req: GetGenresRequest) -> ApiResult<Paged<Genre>> {
        unimpl!("browse_get_genres")
    }
    /// `browse_get_genre`
    fn browse_get_genre(&self, _name: &str) -> ApiResult<Genre> {
        unimpl!("browse_get_genre")
    }
    /// `browse_get_artist_top_tracks`
    fn browse_get_artist_top_tracks(&self, _id: &str) -> ApiResult<ArtistTopTracks> {
        unimpl!("browse_get_artist_top_tracks")
    }
    /// `browse_get_artist_info`
    fn browse_get_artist_info(&self, _id: &str) -> ApiResult<ArtistInfo> {
        unimpl!("browse_get_artist_info")
    }
    /// `history_recent`
    fn history_recent(&self, _req: HistoryRecentRequest) -> ApiResult<Paged<HistoryEntry>> {
        unimpl!("history_recent")
    }
    /// `session_check_server_queue` (Home's "Resume from server" bar; `None` = nothing to offer)
    fn session_check_server_queue(&self) -> ApiResult<Option<ServerQueueOffer>> {
        Ok(None)
    }
    /// `session_resume_from_server`
    fn session_resume_from_server(&self) -> ApiResult<()> {
        unimpl!("session_resume_from_server")
    }
}

/// `search_*`
pub trait SearchApi {
    /// `search_run`
    fn search_run(&self, _req: SearchRequest) -> ApiResult<SearchResults> {
        unimpl!("search_run")
    }
}

/// `playlists_*`
pub trait PlaylistsApi {
    /// `playlists_list`
    fn playlists_list(&self) -> ApiResult<Vec<Playlist>>;
    /// `playlists_get`
    fn playlists_get(&self, _id: &str) -> ApiResult<PlaylistDetail> {
        unimpl!("playlists_get")
    }
    /// `playlists_create`
    fn playlists_create(&self, _req: CreatePlaylistRequest) -> ApiResult<Playlist> {
        unimpl!("playlists_create")
    }
    /// `playlists_rename`
    fn playlists_rename(&self, _req: RenamePlaylistRequest) -> ApiResult<()> {
        unimpl!("playlists_rename")
    }
    /// `playlists_delete`
    fn playlists_delete(&self, _req: DeletePlaylistRequest) -> ApiResult<()> {
        unimpl!("playlists_delete")
    }
    /// `playlists_add_tracks`
    fn playlists_add_tracks(&self, _req: AddToPlaylistRequest) -> ApiResult<()> {
        unimpl!("playlists_add_tracks")
    }
    /// `playlists_remove_tracks`
    fn playlists_remove_tracks(&self, _req: RemovePlaylistTracksRequest) -> ApiResult<()> {
        unimpl!("playlists_remove_tracks")
    }
    /// `playlists_reorder`
    fn playlists_reorder(&self, _req: ReorderPlaylistRequest) -> ApiResult<()> {
        unimpl!("playlists_reorder")
    }
    /// `playlists_duplicate`
    fn playlists_duplicate(&self, _id: &str) -> ApiResult<Playlist> {
        unimpl!("playlists_duplicate")
    }
}

/// `star_*` / `rating_*`
pub trait StarsApi {
    /// `star_add`
    fn star_add(&self, req: StarRequest) -> ApiResult<()>;
    /// `star_remove`
    fn star_remove(&self, req: StarRequest) -> ApiResult<()>;
    /// `rating_set`
    fn rating_set(&self, _req: SetRatingRequest) -> ApiResult<()> {
        unimpl!("rating_set")
    }
}

/// `player_*`
pub trait PlayerApi {
    /// `player_play_queue`
    fn player_play_queue(&self, req: PlayQueueRequest) -> ApiResult<()>;
    /// `player_play_all`
    fn player_play_all(&self, req: PlayAllRequest) -> ApiResult<()>;
    /// `player_play_playlist` (`req.id` = playlist id)
    fn player_play_playlist(&self, _req: PlaySourceRequest) -> ApiResult<()> {
        unimpl!("player_play_playlist")
    }
    /// `player_play_liked`
    fn player_play_liked(&self, _req: PlaySourceRequest) -> ApiResult<()> {
        unimpl!("player_play_liked")
    }
    /// `player_play_smart` (`req.id` = smart playlist id)
    fn player_play_smart(&self, _req: PlaySourceRequest) -> ApiResult<()> {
        unimpl!("player_play_smart")
    }
    /// `player_play_pause`
    fn player_play_pause(&self) -> ApiResult<()>;
    /// `player_play`
    fn player_play(&self) -> ApiResult<()>;
    /// `player_pause`
    fn player_pause(&self) -> ApiResult<()>;
    /// `player_next`
    fn player_next(&self) -> ApiResult<()>;
    /// `player_previous`
    fn player_previous(&self) -> ApiResult<()>;
    /// `player_seek`
    fn player_seek(&self, req: SeekRequest) -> ApiResult<()>;
    /// `player_get_state` (reads the last state, no engine round trip).
    fn player_get_state(&self) -> ApiResult<PlayerState>;
    /// `player_set_spectrum_active`: true only while a spectrum canvas is visible.
    /// While false the backend must not compute or emit spectrum frames.
    fn player_set_spectrum_active(&self, active: bool) -> ApiResult<()>;
    /// `player_set_volume`
    fn player_set_volume(&self, req: SetVolumeRequest) -> ApiResult<()>;
    /// `player_set_shuffle`
    fn player_set_shuffle(&self, enabled: bool) -> ApiResult<()>;
    /// `player_set_repeat`
    fn player_set_repeat(&self, mode: RepeatMode) -> ApiResult<()>;
    /// `player_get_queue` (paged; `total` never reflects a source's whole pool).
    fn player_get_queue(&self, req: PageRequest) -> ApiResult<QueueState>;
    /// `player_jump_to`
    fn player_jump_to(&self, index: usize) -> ApiResult<()>;
    /// `player_enqueue`
    fn player_enqueue(&self, req: EnqueueRequest) -> ApiResult<()>;
    /// `player_get_waveform`
    fn player_get_waveform(&self, _track_id: &str) -> ApiResult<Waveform> {
        unimpl!("player_get_waveform")
    }
    /// `player_play_genre`
    fn player_play_genre(&self, _req: PlayGenreRequest) -> ApiResult<()> {
        unimpl!("player_play_genre")
    }
    /// `player_play_artist`
    fn player_play_artist(&self, _req: PlayArtistRequest) -> ApiResult<()> {
        unimpl!("player_play_artist")
    }
}

/// `output_*`
pub trait OutputApi {
    /// `output_list_devices`
    fn output_list_devices(&self) -> ApiResult<Vec<OutputDevice>> {
        unimpl!("output_list_devices")
    }
    /// `output_set_device`
    fn output_set_device(&self, _id: &str) -> ApiResult<()> {
        unimpl!("output_set_device")
    }
    /// `output_set_mode`
    fn output_set_mode(&self, _mode: OutputMode) -> ApiResult<()> {
        unimpl!("output_set_mode")
    }
    /// `output_get_status`
    fn output_get_status(&self) -> ApiResult<OutputStatus>;
}

/// `settings_*`
pub trait SettingsApi {
    /// `settings_get`
    fn settings_get(&self) -> ApiResult<Settings> {
        unimpl!("settings_get")
    }
    /// One `settings_set_*` command.
    fn settings_set(&self, _change: SettingChange) -> ApiResult<()> {
        unimpl!("settings_set")
    }
    /// `mcp_get_status` (the `settings_set_mcp_*` commands answer with the fresh status too)
    fn mcp_status(&self) -> ApiResult<McpStatus> {
        unimpl!("mcp_status")
    }
    /// `art_cache_status`
    fn art_cache_status(&self) -> ApiResult<ArtCacheStatus> {
        unimpl!("art_cache_status")
    }
    /// `updates_check`
    fn updates_check(&self) -> ApiResult<UpdateInfo> {
        unimpl!("updates_check")
    }
}

/// Cover art. Native replacement for the `sonix://art/{id}?s={size}` protocol:
/// decoded pixels at an exact size (one of [`ART_SIZES`]), served from the
/// backend's disk cache. Called off the UI thread.
pub trait ArtApi {
    fn art_get(&self, cover_art: &str, size: u32) -> ApiResult<ArtPixels>;
}

/// Everything the UI depends on.
pub trait Backend:
    AuthApi + LibraryApi + BrowseApi + SearchApi + PlaylistsApi + StarsApi + PlayerApi + OutputApi + SettingsApi + ArtApi + Send + Sync
{
    /// New event stream (each subscriber gets every event from now on). Dropping
    /// the receiver unsubscribes.
    fn subscribe(&self) -> Receiver<Event>;
    /// `Some("MOCK")` for a backend whose data is generated. The UI shows this in
    /// the title bar: mock data must never be mistaken for real telemetry.
    /// "Now" for relative labels (History day headers). The mock anchors its data on a fixed epoch.
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
    }
    fn mock_label(&self) -> Option<&'static str> {
        None
    }
}
