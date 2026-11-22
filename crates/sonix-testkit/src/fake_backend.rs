//! [`FakeBackend`]: an in-memory `sonix_api::Backend` for UI logic tests.
//!
//! * Canned data lives in a public [`FakeState`] (`fake.state(|s| ...)` to read or change it at any time).
//! * Every call is recorded ([`Call`]: method name + `Debug` of the arguments) before anything else happens, so the
//!   log is in issue order.
//! * [`FakeBackend::fail`] makes one method return an error until [`FakeBackend::heal`].
//! * [`FakeBackend::emit`] pushes an event to every `subscribe()` receiver.
//! * [`FakeBackend::set_hook`] runs a closure on every call before it answers (no lock held). GPUI tests install
//!   `move |_| executor.block(executor.simulate_random_delay())` there: other in-flight backend calls may then finish
//!   first, so `#[gpui::test(iterations = N)]` explores response orderings deterministically per seed.
//!
//! Player commands change the fake queue/player state but emit nothing: a test decides which events arrive and when.
use crate::fixtures::{self, page};
use sonix_api::*;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};

pub type Hook = Arc<dyn Fn(&'static str) + Send + Sync>;

/// One recorded backend call.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub method: &'static str,
    /// `Debug` of the arguments (empty for none).
    pub args: String,
}

/// Everything the fake answers from. Public: tests set it up directly or through the `with_*` helpers.
pub struct FakeState {
    pub session: SessionStatus,
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    /// Album id -> its tracks (`browse_get_album`); albums without an entry answer with no tracks.
    pub album_tracks: HashMap<String, Vec<Track>>,
    pub playlists: Vec<PlaylistDetail>,
    pub genres: Vec<Genre>,
    pub history: Vec<HistoryEntry>,
    pub home: HomeSections,
    pub smart: Vec<SmartSummary>,
    pub queue: Vec<QueueItem>,
    pub queue_source: Option<QueueSourceInfo>,
    pub current: Option<usize>,
    pub player: PlayerState,
    pub settings: Settings,
    pub output: OutputStatus,
    pub devices: Vec<OutputDevice>,
    pub lyrics: HashMap<String, Lyrics>,
    /// Track id -> waveform (`player_get_waveform`); others answer an empty, not-done waveform.
    pub waveforms: HashMap<String, Waveform>,
    pub server_queue: Option<ServerQueueOffer>,
    pub update: UpdateInfo,
    pub now_ms: i64,
    pub suspended: bool,
    pub spectrum_active: bool,
    pub attached_hwnd: Option<isize>,
    next_id: u32,
}

impl Default for FakeState {
    fn default() -> Self {
        Self {
            session: fixtures::session_connected(0),
            tracks: Vec::new(),
            albums: Vec::new(),
            artists: Vec::new(),
            album_tracks: HashMap::new(),
            playlists: Vec::new(),
            genres: Vec::new(),
            history: Vec::new(),
            home: HomeSections { recently_played: Vec::new(), frequent: Vec::new(), newest: Vec::new(), random: Vec::new() },
            smart: Vec::new(),
            queue: Vec::new(),
            queue_source: None,
            current: None,
            player: PlayerState::idle(),
            settings: fixtures::settings(),
            output: OutputStatus {
                device_id: None,
                device_name: Some("Fake Output".into()),
                mode: OutputMode::Shared,
                sample_rate: None,
                bit_depth: None,
                format_label: None,
                bit_perfect: false,
                exclusive_error: None,
            },
            devices: vec![OutputDevice { id: "fake".into(), name: "Fake Output".into(), is_default: true }],
            lyrics: HashMap::new(),
            waveforms: HashMap::new(),
            server_queue: None,
            update: UpdateInfo { available: false, current_version: "0.0.0".into(), version: None, notes: None, date: None },
            // 2026-01-01T00:00:00Z: a fixed "now" so relative labels are stable
            now_ms: 1_767_225_600_000,
            suspended: false,
            spectrum_active: false,
            attached_hwnd: None,
            next_id: 0,
        }
    }
}

impl FakeState {
    fn find_track(&self, id: &str) -> Track {
        self.tracks.iter().find(|t| t.id == id).cloned().unwrap_or_else(|| fixtures::track(id))
    }
    fn playlist(&mut self, id: &str) -> ApiResult<&mut PlaylistDetail> {
        self.playlists.iter_mut().find(|p| p.playlist.id == id).ok_or_else(|| ApiError::not_found("playlist"))
    }
    fn liked(&self) -> Vec<Track> {
        self.tracks.iter().filter(|t| t.starred.is_some()).cloned().collect()
    }
    /// Queue rows replaced by `ids`, playing `start`.
    fn load_queue(&mut self, ids: &[String], start: usize, source: Option<QueueSourceInfo>) {
        self.queue = ids.iter().map(|id| fixtures::queue_item(self.find_track(id))).collect();
        self.queue_source = source;
        self.current = (!self.queue.is_empty()).then(|| start.min(self.queue.len() - 1));
        self.sync_player(PlaybackStatus::Playing);
    }
    fn sync_player(&mut self, status: PlaybackStatus) {
        let track = self.current.and_then(|i| self.queue.get(i)).map(|q| q.track.clone());
        self.player.status = if track.is_some() { status } else { PlaybackStatus::Stopped };
        self.player.duration_sec = track.as_ref().map(|t| t.duration).unwrap_or(0.0);
        self.player.track = track;
        self.player.position_sec = 0.0;
        self.player.queue_length = self.queue.len();
    }
    fn source_play(&mut self, kind: SourceKind, id: Option<String>, label: String, tracks: Vec<Track>, shuffle: bool) {
        let ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
        let mode = if shuffle { SourceMode::Shuffle } else { SourceMode::InOrder };
        self.load_queue(&ids, 0, Some(QueueSourceInfo { kind, label, id, mode, remaining: 0, arc: None, flow_coverage: None }));
    }
    fn new_id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}{}", self.next_id)
    }
}

/// In-memory `Backend`; see the module docs.
#[derive(Default)]
pub struct FakeBackend {
    state: Mutex<FakeState>,
    calls: Mutex<Vec<Call>>,
    errors: Mutex<HashMap<&'static str, ApiError>>,
    subs: Mutex<Vec<Sender<Event>>>,
    hook: Mutex<Option<Hook>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // a panicking test thread must not cascade into every later call
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    // ------------------------------------------------------------------ setup
    pub fn with_tracks(self, tracks: Vec<Track>) -> Self {
        self.state(|s| {
            s.session.track_count = Some(tracks.len() as u64);
            s.tracks = tracks;
        });
        self
    }
    pub fn with_albums(self, albums: Vec<Album>) -> Self {
        self.state(|s| s.albums = albums);
        self
    }
    pub fn with_artists(self, artists: Vec<Artist>) -> Self {
        self.state(|s| s.artists = artists);
        self
    }
    pub fn with_playlists(self, playlists: Vec<PlaylistDetail>) -> Self {
        self.state(|s| s.playlists = playlists);
        self
    }
    /// Queue rows `tracks`, `current` playing (status Playing).
    pub fn with_queue(self, tracks: Vec<Track>, current: usize) -> Self {
        self.state(|s| {
            let ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
            for t in tracks {
                if !s.tracks.iter().any(|x| x.id == t.id) {
                    s.tracks.push(t);
                }
            }
            s.load_queue(&ids, current, None);
        });
        self
    }
    /// Read or change the canned state.
    pub fn state<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        f(&mut lock(&self.state))
    }

    // ------------------------------------------------------------------ calls
    pub fn calls(&self) -> Vec<Call> {
        lock(&self.calls).clone()
    }
    /// Return and clear the call log.
    pub fn take_calls(&self) -> Vec<Call> {
        std::mem::take(&mut *lock(&self.calls))
    }
    pub fn calls_to(&self, method: &str) -> Vec<Call> {
        lock(&self.calls).iter().filter(|c| c.method == method).cloned().collect()
    }
    pub fn count(&self, method: &str) -> usize {
        lock(&self.calls).iter().filter(|c| c.method == method).count()
    }

    // ------------------------------------------------------------------ faults
    /// `method` answers `err` until [`Self::heal`].
    pub fn fail(&self, method: &'static str, err: ApiError) {
        lock(&self.errors).insert(method, err);
    }
    /// Every injected error is removed.
    pub fn heal(&self) {
        lock(&self.errors).clear();
    }
    /// Runs before every answer, with no lock held (delays, gates, counters).
    pub fn set_hook(&self, hook: impl Fn(&'static str) + Send + Sync + 'static) {
        *lock(&self.hook) = Some(Arc::new(hook));
    }
    pub fn clear_hook(&self) {
        *lock(&self.hook) = None;
    }

    // ------------------------------------------------------------------ events
    /// Send `ev` to every live subscriber (dropped receivers are pruned).
    pub fn emit(&self, ev: Event) {
        lock(&self.subs).retain(|tx| tx.send(ev.clone()).is_ok());
    }
    /// Subscribers registered so far (a dropped receiver is pruned by the next `emit`).
    pub fn subscriber_count(&self) -> usize {
        lock(&self.subs).len()
    }
    /// The current player state as an event (what the engine sends after a command).
    pub fn emit_player_state(&self) {
        let st = self.state(|s| s.player.clone());
        self.emit(Event::PlayerState(Box::new(st)));
    }

    /// Record, run the hook (request latency), check for an injected error. Every trait method starts here; the
    /// returned guard runs the hook once more after the answer was computed (response latency), so an answer read
    /// from the state at request time can arrive after the answer to a newer request.
    fn enter(&self, method: &'static str, args: String) -> ApiResult<Leave> {
        lock(&self.calls).push(Call { method, args });
        let hook = lock(&self.hook).clone();
        if let Some(h) = &hook {
            h(method);
        }
        match lock(&self.errors).get(method) {
            Some(e) => Err(e.clone()),
            None => Ok(Leave(hook.map(|h| (h, method)))),
        }
    }
}

/// Runs the hook when dropped: the end of a trait method, after its answer was computed.
struct Leave(Option<(Hook, &'static str)>);

impl Drop for Leave {
    fn drop(&mut self) {
        if let Some((h, m)) = self.0.take() {
            h(m);
        }
    }
}

/// `call!(self, "name", args...)` = `let _leave = self.enter("name", format!("{:?}", (args...)))?;`.
macro_rules! call {
    ($s:ident, $m:literal) => {
        let _leave = $s.enter($m, String::new())?;
    };
    ($s:ident, $m:literal, $($a:expr),+) => {
        let _leave = $s.enter($m, format!("{:?}", ($(&$a),+)))?;
    };
}

fn caps() -> ServerCapabilities {
    ServerCapabilities { server_name: "fake".into(), server_version: "0.0.0".into(), api_version: "1.16.1".into(), open_subsonic_extensions: vec![], ping_ms: 1 }
}

fn sort_tracks(v: &mut [Track], field: TrackSortField, dir: SortDir) {
    let key = |t: &Track| -> String {
        match field {
            TrackSortField::Title => t.title.to_lowercase(),
            TrackSortField::Artist => t.artist.to_lowercase(),
            TrackSortField::Album => t.album.clone().unwrap_or_default().to_lowercase(),
            TrackSortField::Year => format!("{:06}", t.year.unwrap_or(0)),
            TrackSortField::DateAdded => t.date_added.clone().unwrap_or_default(),
            TrackSortField::Duration => format!("{:012.3}", t.duration),
            TrackSortField::Format => t.format.suffix.clone(),
        }
    };
    v.sort_by_key(key);
    if dir == SortDir::Desc {
        v.reverse();
    }
}

fn matches(t: &Track, q: &Option<String>) -> bool {
    match q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        None => true,
        Some(q) => {
            let q = q.to_lowercase();
            t.title.to_lowercase().contains(&q) || t.artist.to_lowercase().contains(&q) || t.album.as_deref().unwrap_or("").to_lowercase().contains(&q)
        }
    }
}

impl AuthApi for FakeBackend {
    fn auth_connect(&self, req: ConnectRequest) -> ApiResult<ServerCapabilities> {
        call!(self, "auth_connect", req.url, req.username);
        self.state(|s| {
            s.session.state = ConnectionState::Connected;
            s.session.server_url = Some(req.url);
            s.session.username = Some(req.username);
        });
        Ok(caps())
    }
    fn auth_probe(&self) -> ApiResult<ServerCapabilities> {
        call!(self, "auth_probe");
        Ok(caps())
    }
    fn auth_logout(&self) -> ApiResult<()> {
        call!(self, "auth_logout");
        self.state(|s| s.session.state = ConnectionState::Disconnected);
        Ok(())
    }
    fn auth_session_status(&self) -> ApiResult<SessionStatus> {
        call!(self, "auth_session_status");
        Ok(self.state(|s| s.session.clone()))
    }
    fn profiles_list(&self) -> ApiResult<Vec<Profile>> {
        call!(self, "profiles_list");
        Ok(self.state(|s| {
            let (url, user) = (s.session.server_url.clone().unwrap_or_default(), s.session.username.clone().unwrap_or_default());
            vec![Profile { id: "p1".into(), url, username: user, active: true }]
        }))
    }
    fn profiles_switch(&self, id: &str) -> ApiResult<ServerCapabilities> {
        call!(self, "profiles_switch", id);
        Ok(caps())
    }
    fn profiles_remove(&self, id: &str) -> ApiResult<()> {
        call!(self, "profiles_remove", id);
        Ok(())
    }
}

impl LibraryApi for FakeBackend {
    fn library_sync_start(&self, req: SyncStartRequest) -> ApiResult<()> {
        call!(self, "library_sync_start", req);
        Ok(())
    }
    fn liked_count(&self) -> ApiResult<u32> {
        call!(self, "liked_count");
        Ok(self.state(|s| s.liked().len() as u32))
    }
    fn liked_get(&self, req: PageRequest) -> ApiResult<Paged<Track>> {
        call!(self, "liked_get", req);
        Ok(self.state(|s| page(&s.liked(), req.offset, req.limit)))
    }
    fn liked_summary(&self) -> ApiResult<LikedSummary> {
        call!(self, "liked_summary");
        Ok(self.state(|s| {
            let l = s.liked();
            LikedSummary { count: l.len(), duration: l.iter().map(|t| t.duration).sum() }
        }))
    }
    fn history_get(&self, req: PageRequest) -> ApiResult<Paged<HistoryItem>> {
        call!(self, "history_get", req);
        Ok(self.state(|s| {
            let rows: Vec<HistoryItem> = s.history.iter().map(|h| HistoryItem::Track { track: Box::new(h.track.clone()), played_at: h.played_at }).collect();
            page(&rows, req.offset, req.limit)
        }))
    }
    fn history_clear(&self) -> ApiResult<()> {
        call!(self, "history_clear");
        self.state(|s| s.history.clear());
        Ok(())
    }
    fn smart_list(&self) -> ApiResult<Vec<SmartSummary>> {
        call!(self, "smart_list");
        Ok(self.state(|s| s.smart.clone()))
    }
    fn smart_get(&self, id: &str, seed: u32) -> ApiResult<SmartSummary> {
        call!(self, "smart_get", id, seed);
        self.state(|s| s.smart.iter().find(|x| x.id == id).cloned()).ok_or_else(|| ApiError::not_found("smart playlist"))
    }
    fn smart_create(&self, input: SmartInput) -> ApiResult<SmartSummary> {
        call!(self, "smart_create", input);
        Ok(self.state(|s| {
            let id = s.new_id("sm");
            let sm = SmartSummary { id, created_at: String::new(), updated_at: String::new(), input, track_count: 0, duration: 0.0 };
            s.smart.push(sm.clone());
            sm
        }))
    }
    fn smart_update(&self, id: &str, input: SmartInput) -> ApiResult<SmartSummary> {
        call!(self, "smart_update", id, input);
        self.state(|s| {
            let sm = s.smart.iter_mut().find(|x| x.id == id).ok_or_else(|| ApiError::not_found("smart playlist"))?;
            sm.input = input;
            Ok(sm.clone())
        })
    }
    fn smart_delete(&self, id: &str) -> ApiResult<()> {
        call!(self, "smart_delete", id);
        self.state(|s| s.smart.retain(|x| x.id != id));
        Ok(())
    }
    fn smart_duplicate(&self, id: &str) -> ApiResult<SmartSummary> {
        call!(self, "smart_duplicate", id);
        self.state(|s| {
            let mut sm = s.smart.iter().find(|x| x.id == id).cloned().ok_or_else(|| ApiError::not_found("smart playlist"))?;
            sm.id = s.new_id("sm");
            sm.input.name = format!("{} (copy)", sm.input.name);
            s.smart.push(sm.clone());
            Ok(sm)
        })
    }
    fn smart_presets(&self) -> ApiResult<Vec<SmartPreset>> {
        call!(self, "smart_presets");
        Ok(Vec::new())
    }
    fn smart_tracks(&self, id: &str, seed: u32, req: PageRequest) -> ApiResult<Paged<Track>> {
        call!(self, "smart_tracks", id, seed, req);
        Ok(Paged { items: Vec::new(), total: 0, offset: req.offset })
    }
    fn smart_preview(&self, input: SmartInput, seed: u32, req: PageRequest) -> ApiResult<Paged<Track>> {
        call!(self, "smart_preview", input, seed, req);
        Ok(Paged { items: Vec::new(), total: 0, offset: req.offset })
    }
}

impl BrowseApi for FakeBackend {
    fn browse_get_tracks(&self, req: GetTracksRequest) -> ApiResult<Paged<Track>> {
        call!(self, "browse_get_tracks", req);
        Ok(self.state(|s| {
            let mut v: Vec<Track> = s.tracks.iter().filter(|t| matches(t, &req.filter)).cloned().collect();
            sort_tracks(&mut v, req.sort, req.dir);
            page(&v, req.offset, req.limit)
        }))
    }
    fn browse_get_artists(&self, req: GetArtistsRequest) -> ApiResult<Paged<Artist>> {
        call!(self, "browse_get_artists", req);
        Ok(self.state(|s| page(&s.artists, req.offset, req.limit)))
    }
    fn browse_get_albums(&self, req: GetAlbumsRequest) -> ApiResult<Paged<Album>> {
        call!(self, "browse_get_albums", req);
        Ok(self.state(|s| page(&s.albums, req.offset, req.limit)))
    }
    fn browse_get_album(&self, id: &str) -> ApiResult<AlbumDetail> {
        call!(self, "browse_get_album", id);
        self.state(|s| {
            let album = s.albums.iter().find(|a| a.id == id).cloned().ok_or_else(|| ApiError::not_found("album"))?;
            let tracks = s.album_tracks.get(id).cloned().unwrap_or_default();
            Ok(AlbumDetail { album, tracks })
        })
    }
    fn browse_get_artist(&self, id: &str) -> ApiResult<ArtistDetail> {
        call!(self, "browse_get_artist", id);
        self.state(|s| {
            let artist = s.artists.iter().find(|a| a.id == id).cloned().ok_or_else(|| ApiError::not_found("artist"))?;
            let albums = s.albums.iter().filter(|a| a.artist_id.as_deref() == Some(id)).cloned().collect();
            Ok(ArtistDetail { artist, albums })
        })
    }
    fn browse_get_artist_tracks(&self, id: &str) -> ApiResult<Vec<Track>> {
        call!(self, "browse_get_artist_tracks", id);
        Ok(self.state(|s| s.tracks.iter().filter(|t| t.artist_id.as_deref() == Some(id)).cloned().collect()))
    }
    fn browse_get_home_sections(&self) -> ApiResult<HomeSections> {
        call!(self, "browse_get_home_sections");
        Ok(self.state(|s| s.home.clone()))
    }
    fn browse_get_genres(&self, req: GetGenresRequest) -> ApiResult<Paged<Genre>> {
        call!(self, "browse_get_genres", req);
        Ok(self.state(|s| page(&s.genres, req.offset, req.limit)))
    }
    fn browse_get_genre(&self, name: &str) -> ApiResult<Genre> {
        call!(self, "browse_get_genre", name);
        self.state(|s| s.genres.iter().find(|g| g.name == name).cloned()).ok_or_else(|| ApiError::not_found("genre"))
    }
    fn browse_get_artist_top_tracks(&self, id: &str) -> ApiResult<ArtistTopTracks> {
        call!(self, "browse_get_artist_top_tracks", id);
        let items: Vec<Track> = self.state(|s| s.tracks.iter().filter(|t| t.artist_id.as_deref() == Some(id)).take(10).cloned().collect());
        Ok(ArtistTopTracks { source: TopSource::MostPlayed, total: items.len(), offset: 0, items })
    }
    fn browse_get_artist_info(&self, id: &str) -> ApiResult<ArtistInfo> {
        call!(self, "browse_get_artist_info", id);
        Ok(ArtistInfo { biography: None, similar: Vec::new() })
    }
    fn history_recent(&self, req: HistoryRecentRequest) -> ApiResult<Paged<HistoryEntry>> {
        call!(self, "history_recent", req);
        Ok(self.state(|s| page(&s.history, 0, req.limit)))
    }
    fn session_check_server_queue(&self) -> ApiResult<Option<ServerQueueOffer>> {
        call!(self, "session_check_server_queue");
        Ok(self.state(|s| s.server_queue.clone()))
    }
    fn session_resume_from_server(&self) -> ApiResult<()> {
        call!(self, "session_resume_from_server");
        Ok(())
    }
}

impl SearchApi for FakeBackend {
    fn search_run(&self, req: SearchRequest) -> ApiResult<SearchResults> {
        call!(self, "search_run", req);
        Ok(self.state(|s| {
            let q = Some(req.text.clone());
            let tracks: Vec<Track> = s.tracks.iter().filter(|t| matches(t, &q)).cloned().collect();
            let lc = req.text.to_lowercase();
            let albums: Vec<Album> = s.albums.iter().filter(|a| a.name.to_lowercase().contains(&lc)).cloned().collect();
            let artists: Vec<Artist> = s.artists.iter().filter(|a| a.name.to_lowercase().contains(&lc)).cloned().collect();
            let playlists: Vec<Playlist> = s.playlists.iter().map(|p| p.playlist.clone()).filter(|p| p.name.to_lowercase().contains(&lc)).collect();
            SearchResults {
                tracks: page(&tracks, req.offset, req.limit),
                albums: page(&albums, req.offset, req.limit),
                artists: page(&artists, req.offset, req.limit),
                playlists: page(&playlists, req.offset, req.limit),
            }
        }))
    }
}

impl PlaylistsApi for FakeBackend {
    fn playlists_list(&self) -> ApiResult<Vec<Playlist>> {
        call!(self, "playlists_list");
        Ok(self.state(|s| s.playlists.iter().map(|p| p.playlist.clone()).collect()))
    }
    fn playlists_get(&self, id: &str) -> ApiResult<PlaylistDetail> {
        call!(self, "playlists_get", id);
        self.state(|s| s.playlist(id).cloned())
    }
    fn playlists_create(&self, req: CreatePlaylistRequest) -> ApiResult<Playlist> {
        call!(self, "playlists_create", req);
        Ok(self.state(|s| {
            let id = s.new_id("pl");
            let tracks = req.track_ids.iter().map(|t| s.find_track(t)).collect();
            let p = fixtures::playlist(&id, &req.name, tracks);
            s.playlists.push(p.clone());
            p.playlist
        }))
    }
    fn playlists_rename(&self, req: RenamePlaylistRequest) -> ApiResult<()> {
        call!(self, "playlists_rename", req);
        self.state(|s| s.playlist(&req.id).map(|p| p.playlist.name = req.name.clone()))
    }
    fn playlists_delete(&self, req: DeletePlaylistRequest) -> ApiResult<()> {
        call!(self, "playlists_delete", req);
        self.state(|s| s.playlists.retain(|p| p.playlist.id != req.id));
        Ok(())
    }
    fn playlists_add_tracks(&self, req: AddToPlaylistRequest) -> ApiResult<()> {
        call!(self, "playlists_add_tracks", req);
        self.state(|s| {
            let add: Vec<Track> = req.track_ids.iter().map(|t| s.find_track(t)).collect();
            let p = s.playlist(&req.id)?;
            p.tracks.extend(add);
            p.playlist.song_count = p.tracks.len() as u32;
            Ok(())
        })
    }
    fn playlists_remove_tracks(&self, req: RemovePlaylistTracksRequest) -> ApiResult<()> {
        call!(self, "playlists_remove_tracks", req);
        self.state(|s| {
            let p = s.playlist(&req.id)?;
            let mut ix = req.indices.clone();
            ix.sort_unstable();
            ix.dedup();
            ix.retain(|i| *i < p.tracks.len());
            for i in ix.into_iter().rev() {
                p.tracks.remove(i);
            }
            p.playlist.song_count = p.tracks.len() as u32;
            Ok(())
        })
    }
    fn playlists_reorder(&self, req: ReorderPlaylistRequest) -> ApiResult<()> {
        call!(self, "playlists_reorder", req);
        self.state(|s| {
            let tracks: Vec<Track> = req.track_ids.iter().map(|t| s.find_track(t)).collect();
            s.playlist(&req.id)?.tracks = tracks;
            Ok(())
        })
    }
    fn playlists_duplicate(&self, id: &str) -> ApiResult<Playlist> {
        call!(self, "playlists_duplicate", id);
        self.state(|s| {
            let src = s.playlist(id)?.clone();
            let new = s.new_id("pl");
            let p = fixtures::playlist(&new, &format!("{} (copy)", src.playlist.name), src.tracks);
            s.playlists.push(p.clone());
            Ok(p.playlist)
        })
    }
}

impl StarsApi for FakeBackend {
    fn star_add(&self, req: StarRequest) -> ApiResult<()> {
        call!(self, "star_add", req);
        self.state(|s| s.tracks.iter_mut().filter(|t| t.id == req.id).for_each(|t| t.starred = Some("2026-01-01T00:00:00Z".into())));
        Ok(())
    }
    fn star_remove(&self, req: StarRequest) -> ApiResult<()> {
        call!(self, "star_remove", req);
        self.state(|s| s.tracks.iter_mut().filter(|t| t.id == req.id).for_each(|t| t.starred = None));
        Ok(())
    }
    fn rating_set(&self, req: SetRatingRequest) -> ApiResult<()> {
        call!(self, "rating_set", req);
        if req.rating > 5 {
            return Err(ApiError::new(ErrorCode::InvalidRequest, "rating out of range"));
        }
        self.state(|s| s.tracks.iter_mut().filter(|t| t.id == req.id).for_each(|t| t.rating = (req.rating > 0).then_some(req.rating)));
        Ok(())
    }
}

impl PlayerApi for FakeBackend {
    fn player_play_queue(&self, req: PlayQueueRequest) -> ApiResult<()> {
        call!(self, "player_play_queue", req);
        self.state(|s| s.load_queue(&req.track_ids, req.start_index, None));
        Ok(())
    }
    fn player_play_all(&self, req: PlayAllRequest) -> ApiResult<()> {
        call!(self, "player_play_all", req);
        self.state(|s| {
            let mut v: Vec<Track> = s.tracks.iter().filter(|t| matches(t, &req.filter)).cloned().collect();
            if let TrackOrder::Sort { field, dir } = req.order {
                sort_tracks(&mut v, field, dir);
            }
            let start = req.start_track_id.as_ref().and_then(|id| v.iter().position(|t| &t.id == id)).unwrap_or(0);
            let shuffle = req.order == TrackOrder::Shuffle;
            s.source_play(SourceKind::AllTracks, None, "All Tracks".into(), v, shuffle);
            s.current = (!s.queue.is_empty()).then_some(start);
            s.sync_player(PlaybackStatus::Playing);
        });
        Ok(())
    }
    fn player_play_playlist(&self, req: PlaySourceRequest) -> ApiResult<()> {
        call!(self, "player_play_playlist", req);
        self.state(|s| {
            let p = s.playlist(&req.id)?.clone();
            s.source_play(SourceKind::Playlist, Some(req.id.clone()), p.playlist.name, p.tracks, req.shuffle);
            Ok(())
        })
    }
    fn player_play_liked(&self, req: PlaySourceRequest) -> ApiResult<()> {
        call!(self, "player_play_liked", req);
        self.state(|s| {
            let l = s.liked();
            s.source_play(SourceKind::Liked, None, "Liked Songs".into(), l, req.shuffle)
        });
        Ok(())
    }
    fn player_play_smart(&self, req: PlaySourceRequest) -> ApiResult<()> {
        call!(self, "player_play_smart", req);
        Ok(())
    }
    fn player_play_pause(&self) -> ApiResult<()> {
        call!(self, "player_play_pause");
        self.state(|s| {
            s.player.status = match s.player.status {
                PlaybackStatus::Playing | PlaybackStatus::Loading => PlaybackStatus::Paused,
                PlaybackStatus::Paused if s.player.track.is_some() => PlaybackStatus::Playing,
                other => other,
            }
        });
        Ok(())
    }
    fn player_play(&self) -> ApiResult<()> {
        call!(self, "player_play");
        self.state(|s| {
            if s.player.track.is_some() {
                s.player.status = PlaybackStatus::Playing;
            }
        });
        Ok(())
    }
    fn player_pause(&self) -> ApiResult<()> {
        call!(self, "player_pause");
        self.state(|s| {
            if s.player.status == PlaybackStatus::Playing {
                s.player.status = PlaybackStatus::Paused;
            }
        });
        Ok(())
    }
    fn player_next(&self) -> ApiResult<()> {
        call!(self, "player_next");
        self.state(|s| {
            if let Some(c) = s.current.filter(|c| c + 1 < s.queue.len()) {
                s.current = Some(c + 1);
                s.sync_player(PlaybackStatus::Playing);
            }
        });
        Ok(())
    }
    fn player_previous(&self) -> ApiResult<()> {
        call!(self, "player_previous");
        self.state(|s| {
            if let Some(c) = s.current.filter(|c| *c > 0) {
                s.current = Some(c - 1);
                s.sync_player(PlaybackStatus::Playing);
            }
        });
        Ok(())
    }
    fn player_seek(&self, req: SeekRequest) -> ApiResult<()> {
        call!(self, "player_seek", req);
        self.state(|s| s.player.position_sec = req.position_sec.clamp(0.0, s.player.duration_sec.max(0.0)));
        Ok(())
    }
    fn player_get_state(&self) -> ApiResult<PlayerState> {
        call!(self, "player_get_state");
        Ok(self.state(|s| s.player.clone()))
    }
    fn player_set_spectrum_active(&self, active: bool) -> ApiResult<()> {
        call!(self, "player_set_spectrum_active", active);
        self.state(|s| s.spectrum_active = active);
        Ok(())
    }
    fn player_set_volume(&self, req: SetVolumeRequest) -> ApiResult<()> {
        call!(self, "player_set_volume", req);
        self.state(|s| s.player.volume = req.volume.clamp(0.0, 1.0));
        Ok(())
    }
    fn player_set_shuffle(&self, enabled: bool) -> ApiResult<()> {
        call!(self, "player_set_shuffle", enabled);
        self.state(|s| s.player.shuffle = enabled);
        Ok(())
    }
    fn player_set_repeat(&self, mode: RepeatMode) -> ApiResult<()> {
        call!(self, "player_set_repeat", mode);
        self.state(|s| s.player.repeat = mode);
        Ok(())
    }
    fn player_get_queue(&self, req: PageRequest) -> ApiResult<QueueState> {
        call!(self, "player_get_queue", req);
        Ok(self.state(|s| {
            let p = page(&s.queue, req.offset, req.limit);
            QueueState { items: p.items, total: p.total, current_index: s.current.map(|c| c as i64).unwrap_or(-1), source: s.queue_source.clone() }
        }))
    }
    fn player_jump_to(&self, index: usize) -> ApiResult<()> {
        call!(self, "player_jump_to", index);
        self.state(|s| {
            if index < s.queue.len() {
                s.current = Some(index);
                s.sync_player(PlaybackStatus::Playing);
            }
        });
        Ok(())
    }
    fn player_enqueue(&self, req: EnqueueRequest) -> ApiResult<()> {
        call!(self, "player_enqueue", req);
        self.state(|s| {
            let items: Vec<QueueItem> =
                req.track_ids.iter().map(|id| QueueItem { track: s.find_track(id), origin: QueueItemOrigin::User, flow: None }).collect();
            let at = match req.position {
                EnqueuePosition::Next => s.current.map(|c| c + 1).unwrap_or(0).min(s.queue.len()),
                EnqueuePosition::End => s.queue.len(),
            };
            s.queue.splice(at..at, items);
            s.player.queue_length = s.queue.len();
        });
        Ok(())
    }
    fn player_get_waveform(&self, track_id: &str) -> ApiResult<Waveform> {
        call!(self, "player_get_waveform", track_id);
        let w = self.state(|s| s.waveforms.get(track_id).cloned());
        Ok(w.unwrap_or_else(|| Waveform { track_id: track_id.into(), buckets: vec![0; WAVEFORM_BUCKETS], filled: 0, done: false }))
    }
    fn player_play_genre(&self, req: PlayGenreRequest) -> ApiResult<()> {
        call!(self, "player_play_genre", req);
        self.state(|s| {
            let v: Vec<Track> = s.tracks.iter().filter(|t| t.genre.as_deref() == Some(req.name.as_str())).cloned().collect();
            s.source_play(SourceKind::Genre, None, req.name.clone(), v, req.shuffle);
        });
        Ok(())
    }
    fn player_play_artist(&self, req: PlayArtistRequest) -> ApiResult<()> {
        call!(self, "player_play_artist", req);
        self.state(|s| {
            let v: Vec<Track> = s.tracks.iter().filter(|t| t.artist_id.as_deref() == Some(req.id.as_str())).cloned().collect();
            let label = v.first().map(|t| t.artist.clone()).unwrap_or_default();
            s.source_play(SourceKind::Artist, Some(req.id.clone()), label, v, req.shuffle);
        });
        Ok(())
    }
    fn player_remove_from_queue(&self, index: usize) -> ApiResult<()> {
        call!(self, "player_remove_from_queue", index);
        self.state(|s| {
            if index >= s.queue.len() {
                return;
            }
            s.queue.remove(index);
            s.current = match s.current {
                Some(c) if c > index => Some(c - 1),
                Some(c) if c == index && c >= s.queue.len() => s.queue.len().checked_sub(1),
                c => c,
            };
            s.player.queue_length = s.queue.len();
        });
        Ok(())
    }
    fn player_reorder_queue(&self, from: usize, to: usize) -> ApiResult<()> {
        call!(self, "player_reorder_queue", from, to);
        self.state(|s| {
            if from >= s.queue.len() || to >= s.queue.len() {
                return;
            }
            let it = s.queue.remove(from);
            s.queue.insert(to, it);
            s.current = s.current.map(|c| {
                if c == from {
                    to
                } else if from < c && c <= to {
                    c - 1
                } else if to <= c && c < from {
                    c + 1
                } else {
                    c
                }
            });
        });
        Ok(())
    }
    fn flow_get_coverage(&self, req: FlowCoverageRequest) -> ApiResult<FlowCoverage> {
        call!(self, "flow_get_coverage", req);
        Ok(FlowCoverage { total: 0, bpm: 0, key: 0, loudness: 0 })
    }
    fn radio_start(&self, req: RadioStartRequest) -> ApiResult<()> {
        call!(self, "radio_start", req);
        Ok(())
    }
    fn updates_check(&self) -> ApiResult<UpdateInfo> {
        call!(self, "updates_check");
        Ok(self.state(|s| s.update.clone()))
    }
    fn updates_install(&self) -> ApiResult<()> {
        call!(self, "updates_install");
        Ok(())
    }
    fn lyrics_get(&self, track_id: &str) -> ApiResult<Option<Lyrics>> {
        call!(self, "lyrics_get", track_id);
        Ok(self.state(|s| s.lyrics.get(track_id).cloned()))
    }
    fn player_loop_cycle(&self) -> ApiResult<()> {
        call!(self, "player_loop_cycle");
        self.state(|s| {
            let p = &mut s.player;
            match (p.loop_a, p.loop_b) {
                (None, _) => p.loop_a = Some(p.position_sec),
                (Some(_), None) => p.loop_b = Some(p.position_sec),
                _ => (p.loop_a, p.loop_b) = (None, None),
            }
        });
        Ok(())
    }
}

impl OutputApi for FakeBackend {
    fn output_list_devices(&self) -> ApiResult<Vec<OutputDevice>> {
        call!(self, "output_list_devices");
        Ok(self.state(|s| s.devices.clone()))
    }
    fn output_set_device(&self, id: &str) -> ApiResult<()> {
        call!(self, "output_set_device", id);
        self.state(|s| s.output.device_id = Some(id.into()));
        Ok(())
    }
    fn output_set_mode(&self, mode: OutputMode) -> ApiResult<()> {
        call!(self, "output_set_mode", mode);
        self.state(|s| s.output.mode = mode);
        Ok(())
    }
    fn output_get_status(&self) -> ApiResult<OutputStatus> {
        call!(self, "output_get_status");
        Ok(self.state(|s| s.output.clone()))
    }
}

impl SettingsApi for FakeBackend {
    fn settings_get(&self) -> ApiResult<Settings> {
        call!(self, "settings_get");
        Ok(self.state(|s| s.settings.clone()))
    }
    fn settings_set(&self, change: SettingChange) -> ApiResult<()> {
        call!(self, "settings_set", change);
        self.state(|s| {
            let st = &mut s.settings;
            match change {
                SettingChange::Scrobbling(v) => st.scrobbling_enabled = v,
                SettingChange::AutoCheckUpdates(v) => st.auto_check_updates = v,
                SettingChange::SpreadArtists(v) => st.spread_artists_in_shuffle = v,
                SettingChange::QueueWindowSize(v) => st.queue_window_size = v,
                SettingChange::Autoplay(v) => st.autoplay_enabled = v,
                SettingChange::SyncPlayQueue(v) => st.sync_play_queue = v,
                SettingChange::ReplayGain(v) => st.replay_gain_mode = v,
                SettingChange::PlayThreshold(v) => st.play_threshold_sec = v,
                SettingChange::TrackOverlay(v) => st.track_overlay = v,
                SettingChange::TrackOverlayCorner(v) => st.track_overlay_corner = v,
                SettingChange::FreeMemoryWhenMinimized(v) => st.free_memory_when_minimized = v,
                SettingChange::McpEnabled(v) => st.mcp_enabled = v,
                SettingChange::McpAllowControl(v) => st.mcp_allow_control = v,
                SettingChange::VerboseLogging(v) => st.verbose_logging = v,
            }
        });
        Ok(())
    }
    fn mcp_status(&self) -> ApiResult<McpStatus> {
        call!(self, "mcp_status");
        Ok(self.state(|s| McpStatus {
            enabled: s.settings.mcp_enabled,
            allow_control: s.settings.mcp_allow_control,
            listening: false,
            bridge_path: None,
            bridge_exists: false,
            register_command: None,
            last_error: None,
        }))
    }
    fn art_cache_status(&self) -> ApiResult<ArtCacheStatus> {
        call!(self, "art_cache_status");
        Ok(ArtCacheStatus { cached: 0, total: 0, bytes: 0 })
    }
}

impl ArtApi for FakeBackend {
    /// A solid grey square of `size` px (decoding is not what UI tests are about).
    fn art_get(&self, cover_art: &str, size: u32) -> ApiResult<ArtPixels> {
        call!(self, "art_get", cover_art, size);
        Ok(ArtPixels { width: size, height: size, rgba: vec![0x80; (size * size * 4) as usize] })
    }
}

impl Backend for FakeBackend {
    fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = channel();
        lock(&self.subs).push(tx);
        rx
    }
    fn now_ms(&self) -> i64 {
        self.state(|s| s.now_ms)
    }
    fn shutdown(&self) {
        lock(&self.calls).push(Call { method: "shutdown", args: String::new() });
    }
    fn set_suspended(&self, suspended: bool) {
        lock(&self.calls).push(Call { method: "set_suspended", args: format!("{suspended:?}") });
        self.state(|s| s.suspended = suspended);
    }
    fn attach_window(&self, hwnd: isize) {
        lock(&self.calls).push(Call { method: "attach_window", args: format!("{hwnd:?}") });
        self.state(|s| s.attached_hwnd = Some(hwnd));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn req(offset: usize) -> GetTracksRequest {
        GetTracksRequest { offset, limit: 100, sort: TrackSortField::Title, dir: SortDir::Asc, filter: None }
    }

    #[test]
    fn pages_record_calls_and_inject_errors() {
        let b = FakeBackend::new().with_tracks(tracks(250));
        let p = b.browse_get_tracks(req(200)).unwrap();
        assert_eq!((p.items.len(), p.total, p.items[0].id.as_str()), (50, 250, "t0200"));
        assert_eq!(b.auth_session_status().unwrap().track_count, Some(250));
        b.fail("browse_get_tracks", ApiError::new(ErrorCode::Network, "down"));
        assert_eq!(b.browse_get_tracks(req(0)).unwrap_err().code, ErrorCode::Network);
        b.heal();
        assert!(b.browse_get_tracks(req(0)).is_ok());
        assert_eq!(b.count("browse_get_tracks"), 3);
        assert!(b.calls_to("browse_get_tracks")[0].args.contains("offset: 200"));
        let desc = b.browse_get_tracks(GetTracksRequest { dir: SortDir::Desc, ..req(0) }).unwrap();
        assert_eq!(desc.items[0].id, "t0249");
    }

    #[test]
    fn events_reach_every_live_subscriber_and_the_hook_runs_per_call() {
        let b = FakeBackend::new();
        let (r1, r2) = (b.subscribe(), b.subscribe());
        drop(r2);
        b.emit(Event::LibraryChanged);
        assert_eq!(r1.try_recv().unwrap(), Event::LibraryChanged);
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        b.set_hook(move |_| {
            n2.fetch_add(1, Ordering::SeqCst);
        });
        b.player_get_state().unwrap();
        b.liked_count().unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 4, "request and response latency: twice per call");
    }

    #[test]
    fn queue_commands_keep_current_on_the_same_row() {
        let b = FakeBackend::new().with_queue(tracks(5), 2);
        let cur = |b: &FakeBackend| b.player_get_queue(PageRequest { offset: 0, limit: 10 }).unwrap().current_index;
        b.player_reorder_queue(2, 4).unwrap();
        assert_eq!(cur(&b), 4);
        b.player_reorder_queue(0, 4).unwrap();
        assert_eq!(cur(&b), 3);
        b.player_remove_from_queue(0).unwrap();
        assert_eq!(cur(&b), 2);
        b.player_enqueue(EnqueueRequest { track_ids: vec!["x".into()], position: EnqueuePosition::Next }).unwrap();
        let q = b.player_get_queue(PageRequest { offset: 0, limit: 10 }).unwrap();
        assert_eq!((q.total, q.items[3].track.id.as_str(), q.items[3].origin), (5, "x", QueueItemOrigin::User));
        b.player_next().unwrap();
        assert_eq!(b.player_get_state().unwrap().track.unwrap().id, "x");
    }
}
