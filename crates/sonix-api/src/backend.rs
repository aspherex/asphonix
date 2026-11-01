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
use crate::error::{ApiError, ApiResult};
use crate::types::*;
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
    /// `player_remove_from_queue`
    fn player_remove_from_queue(&self, _index: usize) -> ApiResult<()> {
        Ok(())
    }
    /// `player_reorder_queue`: move one entry (`mock.ts` splice semantics, `current` follows).
    fn player_reorder_queue(&self, _from: usize, _to: usize) -> ApiResult<()> {
        Ok(())
    }
    /// `flow_get_coverage`: how much of a Flow pool has real sequencing data.
    fn flow_get_coverage(&self, _req: FlowCoverageRequest) -> ApiResult<FlowCoverage> {
        Ok(FlowCoverage { total: 0, bpm: 0, key: 0, loudness: 0 })
    }
    /// `radio_start`: a radio queue seeded from a track, album or artist.
    fn radio_start(&self, _req: RadioStartRequest) -> ApiResult<()> {
        Ok(())
    }
    /// `updates_check`: the newer release, if any (also announced as `Event::UpdateAvailable`).
    fn updates_check(&self) -> ApiResult<UpdateInfo> {
        Ok(UpdateInfo { available: false, current_version: String::new(), version: None, notes: None, date: None })
    }
    /// `updates_install`: download + install + restart (progress as `Event::UpdateProgress`); explicit user action only.
    fn updates_install(&self) -> ApiResult<()> {
        Ok(())
    }
    /// `lyrics_get`: None = no lyrics for the track.
    fn lyrics_get(&self, _track_id: &str) -> ApiResult<Option<Lyrics>> {
        Ok(None)
    }
    /// `fullPlayer.loopCycle`: A-B loop (set A, set B, clear).
    fn player_loop_cycle(&self) -> ApiResult<()> {
        Ok(())
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
    fn mock_label(&self) -> Option<&'static str> {
        None
    }
}
