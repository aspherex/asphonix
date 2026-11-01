//! The mock `Backend`: a deterministic ~54.5k-track library (`library.rs`), a
//! playback *simulation* (position ticks, rolling queue window, synthetic
//! spectrum) and procedural covers. Everything here is labelled MOCK by
//! `Backend::mock_label`; none of it is real telemetry.
//!
//! Threading: one ticker thread, blocked on a condvar (zero wake-ups) unless a track
//! is playing; spectrum frames are only produced while `player_set_spectrum_active(true)`.
use crate::library::{Library, track_index};
use crate::art;
use sonix_api::*;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// Rolling queue window (Settings "Queue window", default 20).
pub const QUEUE_WINDOW: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PermKey {
    sort: TrackSortField,
    dir: SortDir,
    filter: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    S(String),
    N(i64),
}

#[derive(Clone)]
struct Entry {
    ix: u32,
    origin: QueueItemOrigin,
}

struct Source {
    kind: SourceKind,
    label: String,
    mode: SourceMode,
    remaining: VecDeque<u32>,
}

struct Core {
    entries: Vec<Entry>,
    current: Option<usize>,
    status: PlaybackStatus,
    pos: f64,
    volume: f64,
    shuffle: bool,
    repeat: RepeatMode,
    source: Option<Source>,
    window: usize,
    rng: u64,
    output_mode: OutputMode,
}

struct Inner {
    lib: Library,
    perm: Mutex<Option<(PermKey, Arc<Vec<u32>>)>>,
    core: Mutex<Core>,
    cv: Condvar,
    subs: Mutex<Vec<Sender<Event>>>,
    spectrum_active: AtomicBool,
    shutdown: AtomicBool,
    starred: Mutex<HashMap<String, String>>,
    playlists: Vec<(Playlist, Vec<u32>)>,
}

pub struct MockBackend {
    inner: Arc<Inner>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn xorshift(s: &mut u64) -> u64 {
    let mut x = *s;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *s = x;
    x
}

fn shuffle(v: &mut [u32], rng: &mut u64) {
    for i in (1..v.len()).rev() {
        let j = (xorshift(rng) % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
}

pub fn volume_db(volume: f64) -> Option<f64> {
    if volume > 0.0 { Some(60.0 * (volume.min(1.0) - 1.0)) } else { None }
}

impl MockBackend {
    pub fn new() -> Self {
        let lib = Library::new();
        let total = lib.total_tracks();
        let names = ["Late Night Code", "Heavy Modular Synth", "Nordic Ambient", "90s Drill & Bass", "IDM Essentials"];
        let mut playlists = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let ids: Vec<u32> = (0..12 + (i % 5)).map(|j| ((i * 777 + j * 13) % total) as u32).collect();
            playlists.push((Self::playlist_meta(&lib, format!("pl_{i}"), name, &ids), ids));
        }
        // The big one that exercises the rolling queue window (SoniX mocks it as "Spotify Import (1,980)").
        let big: Vec<u32> = (0..1980usize).map(|j| ((j * 27 + 5) % total) as u32).collect();
        playlists.push((Self::playlist_meta(&lib, "pl_big".into(), "Spotify Import (1,980)", &big), big));

        let inner = Arc::new(Inner {
            lib,
            perm: Mutex::new(None),
            core: Mutex::new(Core {
                entries: Vec::new(),
                current: None,
                status: PlaybackStatus::Stopped,
                pos: 0.0,
                volume: 1.0,
                shuffle: false,
                repeat: RepeatMode::Off,
                source: None,
                window: QUEUE_WINDOW,
                rng: 0x9e37_79b9_7f4a_7c15 ^ std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1),
                output_mode: OutputMode::Exclusive,
            }),
            cv: Condvar::new(),
            subs: Mutex::new(Vec::new()),
            spectrum_active: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            starred: Mutex::new(HashMap::new()),
            playlists,
        });
        let weak = Arc::downgrade(&inner);
        std::thread::Builder::new().name("mock-ticker".into()).spawn(move || ticker(weak)).expect("spawn ticker");
        Self { inner }
    }

    fn playlist_meta(lib: &Library, id: String, name: &str, ids: &[u32]) -> Playlist {
        Playlist {
            id,
            name: name.into(),
            comment: None,
            owner: Some("architect".into()),
            public: false,
            song_count: ids.len() as u32,
            duration: ids.iter().map(|i| lib.sort_fields(*i as usize).duration as f64).sum(),
            cover_art: None,
            created_at: None,
            updated_at: None,
            smart: false,
            read_only: false,
        }
    }

    pub fn total_tracks(&self) -> usize {
        self.inner.lib.total_tracks()
    }

    /// Sorted, filtered permutation of all track indices; one (sort, dir, filter) is cached.
    pub fn permutation(&self, sort: TrackSortField, dir: SortDir, filter: Option<&str>) -> Arc<Vec<u32>> {
        self.inner.permutation(sort, dir, filter)
    }
}

impl Drop for MockBackend {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.cv.notify_all();
    }
}

impl Inner {
    fn permutation(&self, sort: TrackSortField, dir: SortDir, filter: Option<&str>) -> Arc<Vec<u32>> {
        let key = PermKey { sort, dir, filter: filter.unwrap_or("").trim().to_lowercase() };
        if let Some((k, p)) = self.perm.lock().unwrap().as_ref() {
            if *k == key {
                return p.clone();
            }
        }
        let total = self.lib.total_tracks();
        let mut keyed: Vec<(Key, u32)> = Vec::new();
        for i in 0..total {
            if !self.lib.matches(i, &key.filter) {
                continue;
            }
            let f = self.lib.sort_fields(i);
            let k = match sort {
                TrackSortField::Title => Key::S(f.title.to_lowercase()),
                TrackSortField::Artist => Key::S(f.artist.to_lowercase()),
                TrackSortField::Album => Key::S(f.album.to_lowercase()),
                TrackSortField::Year => Key::N(f.year as i64),
                TrackSortField::DateAdded => Key::N(f.added),
                TrackSortField::Duration => Key::N(f.duration as i64),
                TrackSortField::Format => Key::S(f.format),
            };
            keyed.push((k, i as u32));
        }
        // Stable: ties keep ascending library order in both directions (like Array.sort).
        keyed.sort_by(|a, b| {
            let o = a.0.cmp(&b.0);
            if dir == SortDir::Desc { o.reverse() } else { o }
        });
        let p = Arc::new(keyed.into_iter().map(|(_, i)| i).collect::<Vec<_>>());
        *self.perm.lock().unwrap() = Some((key, p.clone()));
        p
    }

    fn track(&self, ix: u32) -> Track {
        let mut t = self.lib.track(ix as usize);
        if let Some(s) = self.starred.lock().unwrap().get(&t.id) {
            t.starred = Some(s.clone());
        }
        t
    }

    fn emit(&self, ev: Event) {
        self.subs.lock().unwrap().retain(|s| s.send(ev.clone()).is_ok());
    }

    fn state(&self, c: &Core) -> PlayerState {
        let track = c.current.and_then(|i| c.entries.get(i)).map(|e| self.track(e.ix));
        let shared = c.output_mode == OutputMode::Shared;
        let mut s = PlayerState::idle();
        s.status = c.status;
        s.position_sec = c.pos;
        s.duration_sec = track.as_ref().map(|t| t.duration).unwrap_or(0.0);
        s.output_mode = c.output_mode;
        s.sample_rate = track.as_ref().map(|t| t.format.sampling_rate);
        s.bit_depth = if shared { Some(32) } else { track.as_ref().and_then(|t| t.format.bit_depth) };
        s.float_mix = if shared && track.is_some() { Some(true) } else { None };
        s.format_label = track.as_ref().map(|t| t.format.suffix.to_uppercase());
        s.volume = c.volume;
        s.volume_db = volume_db(c.volume);
        s.bit_perfect = track.is_some() && c.status != PlaybackStatus::Stopped && !shared && c.volume >= 1.0;
        s.shuffle = c.shuffle;
        s.repeat = c.repeat;
        s.queue_length = c.entries.len();
        s.track = track;
        s
    }

    fn push_state(&self, c: &Core) {
        self.emit(Event::PlayerState(Box::new(self.state(c))));
    }

    /// Draw from the source until `window` ids are queued ahead of `current`.
    fn refill(c: &mut Core) {
        let Some(src) = c.source.as_mut() else { return };
        let ahead = |c: &Core| c.entries.len().saturating_sub(c.current.map(|i| i + 1).unwrap_or(0));
        let _ = ahead;
        let cur = c.current.map(|i| i + 1).unwrap_or(0);
        while c.entries.len().saturating_sub(cur) < c.window {
            match src.remaining.pop_front() {
                Some(ix) => c.entries.push(Entry { ix, origin: QueueItemOrigin::Source }),
                None => break,
            }
        }
        // prune history to ~window tracks behind the current one
        if let Some(cu) = c.current {
            let keep_from = cu.saturating_sub(c.window);
            if keep_from > 0 {
                c.entries.drain(..keep_from);
                c.current = Some(cu - keep_from);
            }
        }
    }

    fn start_current(c: &mut Core) {
        c.pos = 0.0;
        c.status = PlaybackStatus::Playing;
    }

    /// Mirrors `Queue::next`.
    fn advance(c: &mut Core) -> bool {
        if c.entries.is_empty() {
            return false;
        }
        if c.repeat == RepeatMode::One && c.current.is_some() {
            return true;
        }
        let next = c.current.map(|i| i + 1).unwrap_or(0);
        Self::refill(c);
        let next = if c.current.is_some() { c.current.unwrap() + 1 } else { next };
        if next < c.entries.len() {
            c.current = Some(next);
            Self::refill(c);
            return true;
        }
        if c.repeat == RepeatMode::All && c.source.is_none() {
            c.current = Some(0);
            return true;
        }
        c.current = None;
        false
    }

    fn set_source(&self, c: &mut Core, kind: SourceKind, label: String, mode: SourceMode, pool: Vec<u32>, start: Option<u32>) {
        let mut pool: VecDeque<u32> = pool.into();
        if let Some(s) = start {
            // the start track opens the queue; in order mode the pool continues after it
            if let Some(p) = pool.iter().position(|x| *x == s) {
                if mode == SourceMode::Shuffle {
                    pool.remove(p);
                } else {
                    pool.drain(..=p);
                }
            } else {
                pool.retain(|x| *x != s);
            }
            c.entries = vec![Entry { ix: s, origin: QueueItemOrigin::Source }];
        } else {
            c.entries = Vec::new();
        }
        c.source = Some(Source { kind, label, mode, remaining: pool });
        if c.entries.is_empty() {
            if let Some(first) = c.source.as_mut().and_then(|s| s.remaining.pop_front()) {
                c.entries.push(Entry { ix: first, origin: QueueItemOrigin::Source });
            }
        }
        c.current = if c.entries.is_empty() { None } else { Some(0) };
        Self::refill(c);
        if c.current.is_some() {
            Self::start_current(c);
        }
    }
}

fn ticker(weak: Weak<Inner>) {
    let mut last = Instant::now();
    let mut state_acc = 0.0f64;
    let mut phase = 0.0f32;
    loop {
        let Some(inner) = weak.upgrade() else { return };
        let mut core: MutexGuard<Core> = inner.core.lock().unwrap();
        // Fully blocked (no timers at all) unless playing.
        while core.status != PlaybackStatus::Playing && !inner.shutdown.load(Ordering::SeqCst) {
            core = inner.cv.wait(core).unwrap();
            last = Instant::now();
        }
        if inner.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let (c2, _) = inner.cv.wait_timeout(core, Duration::from_millis(50)).unwrap();
        core = c2;
        if core.status != PlaybackStatus::Playing {
            continue;
        }
        let dt = last.elapsed().as_secs_f64();
        last = Instant::now();
        core.pos += dt;
        state_acc += dt;
        let dur = core.current.and_then(|i| core.entries.get(i)).map(|e| inner.lib.sort_fields(e.ix as usize).duration as f64).unwrap_or(0.0);
        let mut queue_changed = false;
        if dur > 0.0 && core.pos >= dur {
            if Inner::advance(&mut core) {
                core.pos = 0.0;
            } else {
                core.status = PlaybackStatus::Stopped;
                core.pos = 0.0;
            }
            state_acc = 1.0;
            queue_changed = true;
        }
        if state_acc >= 0.25 {
            state_acc = 0.0;
            inner.push_state(&core);
            if queue_changed {
                inner.emit(Event::QueueChanged);
            }
        }
        drop(core);
        if inner.spectrum_active.load(Ordering::Relaxed) {
            phase += 0.05;
            inner.emit(Event::Spectrum(synth_spectrum(phase)));
        }
    }
}

/// Synthetic spectrum stand-in (MOCK): never real telemetry.
pub fn synth_spectrum(t: f32) -> SpectrumFrame {
    const B: usize = SPECTRUM_BANDS;
    let kick = (t * 2.2).sin().max(0.0).powi(3);
    let mut bands = Vec::with_capacity(B);
    for i in 0..B {
        let f = i as f32 / B as f32;
        let base = (1.0 - f * 0.75) * (0.45 + 0.4 * (t * (1.1 + f * 3.0) + i as f32 * 0.7).sin().abs());
        let v = base + kick * (1.0 - f).powi(2) * 0.35 + 0.05 * (t * 17.0 + i as f32 * 3.1).sin();
        bands.push((v.clamp(0.0, 1.0) * 255.0) as u8);
    }
    let db = |a: f32| 20.0 * a.clamp(0.02, 1.0).log10();
    SpectrumFrame {
        bands,
        peak_l: db(0.55 + 0.4 * (t * 1.7).sin() + kick * 0.1),
        peak_r: db(0.5 + 0.4 * (t * 1.3 + 1.0).sin() + kick * 0.1),
    }
}

// ------------------------------------------------------------------ trait impls
impl AuthApi for MockBackend {
    fn auth_session_status(&self) -> ApiResult<SessionStatus> {
        Ok(SessionStatus {
            state: ConnectionState::Connected,
            server_url: Some("mock://sonix".into()),
            username: Some("architect".into()),
            server_version: Some("mock".into()),
            api_version: Some("1.16.1".into()),
            ping_ms: Some(12),
            last_sync_at: None,
            track_count: Some(self.inner.lib.total_tracks() as u64),
        })
    }
    fn auth_probe(&self) -> ApiResult<ServerCapabilities> {
        Ok(ServerCapabilities {
            server_name: "SONIX mock".into(),
            server_version: "mock".into(),
            api_version: "1.16.1".into(),
            open_subsonic_extensions: vec![],
            ping_ms: 12,
        })
    }
}

impl LibraryApi for MockBackend {}
impl SearchApi for MockBackend {}
impl SettingsApi for MockBackend {
    fn settings_get(&self) -> ApiResult<Settings> {
        Ok(Settings {
            scrobbling_enabled: false,
            auto_check_updates: false,
            spread_artists_in_shuffle: false,
            queue_window_size: QUEUE_WINDOW as u32,
            autoplay_enabled: false,
            sync_play_queue: false,
            play_threshold_sec: 30,
            free_memory_when_minimized: true,
        })
    }
}

impl BrowseApi for MockBackend {
    fn browse_get_tracks(&self, req: GetTracksRequest) -> ApiResult<Paged<Track>> {
        let perm = self.inner.permutation(req.sort, req.dir, req.filter.as_deref());
        let total = perm.len();
        let end = (req.offset + req.limit.min(500)).min(total);
        let start = req.offset.min(end);
        let items = perm[start..end].iter().map(|i| self.inner.track(*i)).collect();
        Ok(Paged { items, total, offset: req.offset })
    }

    fn browse_get_artists(&self, req: GetArtistsRequest) -> ApiResult<Paged<Artist>> {
        let lib = &self.inner.lib;
        let mut ix: Vec<usize> = (0..crate::library::NUM_ARTISTS).collect();
        ix.sort_by_key(|i| crate::library::artist_name(*i).to_lowercase());
        let end = (req.offset + req.limit).min(ix.len());
        let items = ix[req.offset.min(end)..end].iter().map(|i| lib.artist(*i)).collect();
        Ok(Paged { items, total: ix.len(), offset: req.offset })
    }

    fn browse_get_albums(&self, req: GetAlbumsRequest) -> ApiResult<Paged<Album>> {
        let total = crate::library::NUM_ALBUMS;
        let end = (req.offset + req.limit).min(total);
        let items = (req.offset.min(end)..end).map(|i| self.inner.lib.album(i)).collect();
        Ok(Paged { items, total, offset: req.offset })
    }
}

impl PlaylistsApi for MockBackend {
    fn playlists_list(&self) -> ApiResult<Vec<Playlist>> {
        Ok(self.inner.playlists.iter().map(|(p, _)| p.clone()).collect())
    }
    fn playlists_get(&self, id: &str) -> ApiResult<PlaylistDetail> {
        let (p, ids) = self.inner.playlists.iter().find(|(p, _)| p.id == id).ok_or_else(|| ApiError::not_found("playlist"))?;
        Ok(PlaylistDetail { playlist: p.clone(), tracks: ids.iter().map(|i| self.inner.track(*i)).collect() })
    }
}

impl StarsApi for MockBackend {
    fn star_add(&self, req: StarRequest) -> ApiResult<()> {
        self.inner.starred.lock().unwrap().insert(req.id, crate::library::iso_from_ms(crate::library::EPOCH_MS));
        let c = self.inner.core.lock().unwrap();
        self.inner.push_state(&c);
        drop(c);
        self.inner.emit(Event::LibraryChanged);
        Ok(())
    }
    fn star_remove(&self, req: StarRequest) -> ApiResult<()> {
        self.inner.starred.lock().unwrap().remove(&req.id);
        let c = self.inner.core.lock().unwrap();
        self.inner.push_state(&c);
        drop(c);
        self.inner.emit(Event::LibraryChanged);
        Ok(())
    }
}

impl PlayerApi for MockBackend {
    fn player_play_queue(&self, req: PlayQueueRequest) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        c.source = None;
        c.entries = req
            .track_ids
            .iter()
            .filter_map(|id| track_index(id))
            .map(|i| Entry { ix: i as u32, origin: QueueItemOrigin::Source })
            .collect();
        c.current = if c.entries.is_empty() { None } else { Some(req.start_index.min(c.entries.len() - 1)) };
        if c.current.is_some() {
            Inner::start_current(&mut c);
        }
        self.inner.push_state(&c);
        drop(c);
        self.inner.cv.notify_all();
        self.inner.emit(Event::QueueChanged);
        Ok(())
    }

    fn player_play_all(&self, req: PlayAllRequest) -> ApiResult<()> {
        let (sort, dir, shuffle_all) = match req.order {
            TrackOrder::Shuffle => (TrackSortField::Title, SortDir::Asc, true),
            TrackOrder::Sort { field, dir } => (field, dir, false),
        };
        let perm = self.inner.permutation(sort, dir, req.filter.as_deref());
        let mut pool: Vec<u32> = (*perm).clone();
        let start = req.start_track_id.as_deref().and_then(track_index).map(|i| i as u32);
        let mut c = self.inner.core.lock().unwrap();
        if shuffle_all {
            let mut rng = c.rng;
            shuffle(&mut pool, &mut rng);
            c.rng = rng;
        }
        c.shuffle = shuffle_all;
        let mode = if shuffle_all { SourceMode::Shuffle } else { SourceMode::InOrder };
        let label = match req.filter.as_deref().map(str::trim) {
            Some(f) if !f.is_empty() => format!("Tracks matching \"{f}\""),
            _ => "All tracks".into(),
        };
        self.inner.set_source(&mut c, SourceKind::AllTracks, label, mode, pool, start);
        self.inner.push_state(&c);
        drop(c);
        self.inner.cv.notify_all();
        self.inner.emit(Event::QueueChanged);
        Ok(())
    }

    fn player_play_pause(&self) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        match c.status {
            PlaybackStatus::Playing => c.status = PlaybackStatus::Paused,
            PlaybackStatus::Paused => c.status = PlaybackStatus::Playing,
            _ => {
                if c.current.is_none() && !c.entries.is_empty() {
                    c.current = Some(0);
                }
                if c.current.is_some() {
                    Inner::start_current(&mut c);
                }
            }
        }
        self.inner.push_state(&c);
        drop(c);
        self.inner.cv.notify_all();
        Ok(())
    }

    fn player_play(&self) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        if c.current.is_some() && c.status != PlaybackStatus::Playing {
            c.status = PlaybackStatus::Playing;
        }
        self.inner.push_state(&c);
        drop(c);
        self.inner.cv.notify_all();
        Ok(())
    }

    fn player_pause(&self) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        if c.status == PlaybackStatus::Playing {
            c.status = PlaybackStatus::Paused;
        }
        self.inner.push_state(&c);
        Ok(())
    }

    fn player_next(&self) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        if Inner::advance(&mut c) {
            Inner::start_current(&mut c);
        } else {
            c.status = PlaybackStatus::Stopped;
            c.pos = 0.0;
        }
        self.inner.push_state(&c);
        drop(c);
        self.inner.cv.notify_all();
        self.inner.emit(Event::QueueChanged);
        Ok(())
    }

    fn player_previous(&self) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        if c.pos > 3.0 {
            c.pos = 0.0;
        } else if let Some(i) = c.current {
            if i > 0 {
                c.current = Some(i - 1);
            }
            c.pos = 0.0;
        }
        self.inner.push_state(&c);
        drop(c);
        self.inner.emit(Event::QueueChanged);
        Ok(())
    }

    fn player_seek(&self, req: SeekRequest) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        let dur = c.current.and_then(|i| c.entries.get(i)).map(|e| self.inner.lib.sort_fields(e.ix as usize).duration as f64).unwrap_or(0.0);
        c.pos = req.position_sec.clamp(0.0, dur);
        self.inner.push_state(&c);
        Ok(())
    }

    fn player_get_state(&self) -> ApiResult<PlayerState> {
        let c = self.inner.core.lock().unwrap();
        Ok(self.inner.state(&c))
    }

    fn player_set_spectrum_active(&self, active: bool) -> ApiResult<()> {
        self.inner.spectrum_active.store(active, Ordering::SeqCst);
        self.inner.cv.notify_all();
        Ok(())
    }

    fn player_set_volume(&self, req: SetVolumeRequest) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        c.volume = req.volume.clamp(0.0, 1.0);
        self.inner.push_state(&c);
        Ok(())
    }

    fn player_set_shuffle(&self, enabled: bool) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        c.shuffle = enabled;
        self.inner.push_state(&c);
        Ok(())
    }

    fn player_set_repeat(&self, mode: RepeatMode) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        c.repeat = mode;
        self.inner.push_state(&c);
        Ok(())
    }

    fn player_get_queue(&self, req: PageRequest) -> ApiResult<QueueState> {
        let c = self.inner.core.lock().unwrap();
        let end = (req.offset + req.limit).min(c.entries.len());
        let items = c.entries[req.offset.min(end)..end]
            .iter()
            .map(|e| QueueItem { track: self.inner.track(e.ix), origin: e.origin })
            .collect();
        Ok(QueueState {
            items,
            total: c.entries.len(),
            current_index: c.current.map(|i| i as i64).unwrap_or(-1),
            source: c.source.as_ref().map(|s| QueueSourceInfo { kind: s.kind, label: s.label.clone(), id: None, mode: s.mode, remaining: s.remaining.len() }),
        })
    }

    fn player_jump_to(&self, index: usize) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        if index < c.entries.len() {
            c.current = Some(index);
            Inner::refill(&mut c);
            Inner::start_current(&mut c);
        }
        self.inner.push_state(&c);
        drop(c);
        self.inner.cv.notify_all();
        self.inner.emit(Event::QueueChanged);
        Ok(())
    }

    fn player_enqueue(&self, req: EnqueueRequest) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        let ids: Vec<Entry> = req.track_ids.iter().filter_map(|i| track_index(i)).map(|i| Entry { ix: i as u32, origin: QueueItemOrigin::User }).collect();
        let at = match req.position {
            EnqueuePosition::Next => c.current.map(|i| i + 1).unwrap_or(0),
            EnqueuePosition::End => {
                // end of the contiguous user run after `current`, still ahead of source entries
                let mut p = c.current.map(|i| i + 1).unwrap_or(0);
                while p < c.entries.len() && c.entries[p].origin == QueueItemOrigin::User {
                    p += 1;
                }
                p
            }
        };
        let at = at.min(c.entries.len());
        c.entries.splice(at..at, ids);
        self.inner.push_state(&c);
        drop(c);
        self.inner.emit(Event::QueueChanged);
        Ok(())
    }

    fn player_get_waveform(&self, track_id: &str) -> ApiResult<Waveform> {
        let ix = track_index(track_id).ok_or_else(|| ApiError::not_found("track"))?;
        Ok(Waveform { track_id: track_id.into(), buckets: synth_waveform(ix), filled: WAVEFORM_BUCKETS, done: true })
    }
}

/// Deterministic 240-bucket RMS-like envelope (MOCK).
pub fn synth_waveform(ix: usize) -> Vec<u8> {
    let mut r = crate::library::Rng::for_seed(ix as u64 * 11 + 3);
    let (p1, p2, p3) = (r.next() * 6.28, r.next() * 6.28, r.next() * 6.28);
    (0..WAVEFORM_BUCKETS)
        .map(|i| {
            let x = i as f64 / WAVEFORM_BUCKETS as f64;
            let env = (x * std::f64::consts::PI).sin().powf(0.35); // fade in/out
            let v = 0.45 + 0.25 * (x * 9.0 + p1).sin() + 0.18 * (x * 31.0 + p2).sin() + 0.08 * (x * 97.0 + p3).sin();
            let jitter = r.next() * 0.12;
            ((v * env + jitter).clamp(0.04, 1.0) * 255.0) as u8
        })
        .collect()
}

impl OutputApi for MockBackend {
    fn output_get_status(&self) -> ApiResult<OutputStatus> {
        let c = self.inner.core.lock().unwrap();
        let st = self.inner.state(&c);
        Ok(OutputStatus {
            device_id: Some("mock".into()),
            device_name: Some("MOCK OUTPUT (no audio)".into()),
            mode: c.output_mode,
            sample_rate: st.sample_rate,
            bit_depth: st.bit_depth,
            format_label: st.format_label,
            bit_perfect: st.bit_perfect,
            exclusive_error: None,
        })
    }
    fn output_list_devices(&self) -> ApiResult<Vec<OutputDevice>> {
        Ok(vec![OutputDevice { id: "mock".into(), name: "MOCK OUTPUT (no audio)".into(), is_default: true }])
    }
    fn output_set_mode(&self, mode: OutputMode) -> ApiResult<()> {
        let mut c = self.inner.core.lock().unwrap();
        c.output_mode = mode;
        self.inner.push_state(&c);
        Ok(())
    }
}

impl ArtApi for MockBackend {
    fn art_get(&self, cover_art: &str, size: u32) -> ApiResult<ArtPixels> {
        let n: usize = cover_art.get(3..).and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::not_found("cover"))?;
        Ok(art::procedural_cover(n, cover_art.starts_with("ar_"), size))
    }
}

impl Backend for MockBackend {
    fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = channel();
        self.inner.subs.lock().unwrap().push(tx);
        rx
    }
    fn mock_label(&self) -> Option<&'static str> {
        Some("MOCK")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(sort: TrackSortField, dir: SortDir, filter: Option<&str>, offset: usize, limit: usize) -> GetTracksRequest {
        GetTracksRequest { offset, limit, sort, dir, filter: filter.map(Into::into) }
    }

    #[test]
    fn paging_is_consistent_and_deterministic() {
        let b = MockBackend::new();
        let a = b.browse_get_tracks(req(TrackSortField::Title, SortDir::Asc, None, 0, 100)).unwrap();
        assert_eq!(a.total, 54_515);
        assert_eq!(a.items.len(), 100);
        // page 1 + page 2 == one 200-row page
        let p2 = b.browse_get_tracks(req(TrackSortField::Title, SortDir::Asc, None, 100, 100)).unwrap();
        let both = b.browse_get_tracks(req(TrackSortField::Title, SortDir::Asc, None, 0, 200)).unwrap();
        assert_eq!(both.items[..100], a.items[..]);
        assert_eq!(both.items[100..], p2.items[..]);
        // a second backend instance yields identical rows
        let b2 = MockBackend::new();
        assert_eq!(b2.browse_get_tracks(req(TrackSortField::Title, SortDir::Asc, None, 100, 100)).unwrap().items, p2.items);
        // last partial page
        let last = b.browse_get_tracks(req(TrackSortField::Title, SortDir::Asc, None, 54_500, 100)).unwrap();
        assert_eq!(last.items.len(), 15);
        let past = b.browse_get_tracks(req(TrackSortField::Title, SortDir::Asc, None, 60_000, 100)).unwrap();
        assert!(past.items.is_empty());
    }

    #[test]
    fn sort_is_ordered_and_desc_reverses_keys() {
        let b = MockBackend::new();
        let asc = b.browse_get_tracks(req(TrackSortField::Title, SortDir::Asc, None, 0, 200)).unwrap();
        assert!(asc.items.windows(2).all(|w| w[0].title.to_lowercase() <= w[1].title.to_lowercase()));
        let desc = b.browse_get_tracks(req(TrackSortField::Title, SortDir::Desc, None, 0, 200)).unwrap();
        assert!(desc.items.windows(2).all(|w| w[0].title.to_lowercase() >= w[1].title.to_lowercase()));
        let yr = b.browse_get_tracks(req(TrackSortField::Year, SortDir::Asc, None, 0, 200)).unwrap();
        assert!(yr.items.windows(2).all(|w| w[0].year <= w[1].year));
        let added = b.browse_get_tracks(req(TrackSortField::DateAdded, SortDir::Desc, None, 0, 200)).unwrap();
        assert!(added.items.windows(2).all(|w| w[0].date_added >= w[1].date_added));
        // a stable sort: equal keys keep ascending library order
        let ids: Vec<usize> = yr.items.iter().filter(|t| t.year == yr.items[0].year).map(|t| track_index(&t.id).unwrap()).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn filter_narrows_total_and_matches_every_row() {
        let b = MockBackend::new();
        let r = b.browse_get_tracks(req(TrackSortField::Artist, SortDir::Asc, Some("  Overmono "), 0, 100)).unwrap();
        assert!(r.total > 0 && r.total < 54_515);
        for t in &r.items {
            let hay = format!("{} {} {}", t.title, t.artist, t.album.clone().unwrap()).to_lowercase();
            assert!(hay.contains("overmono"));
        }
        let none = b.browse_get_tracks(req(TrackSortField::Artist, SortDir::Asc, Some("zzzq-nothing"), 0, 100)).unwrap();
        assert_eq!(none.total, 0);
    }

    #[test]
    fn play_all_opens_a_rolling_window_and_next_advances() {
        let b = MockBackend::new();
        b.player_play_all(PlayAllRequest {
            order: TrackOrder::Sort { field: TrackSortField::Title, dir: SortDir::Asc },
            filter: None,
            start_track_id: None,
        })
        .unwrap();
        let q = b.player_get_queue(PageRequest { offset: 0, limit: 100 }).unwrap();
        assert!(q.total <= QUEUE_WINDOW + 1, "{}", q.total);
        let src = q.source.unwrap();
        assert_eq!(src.kind, SourceKind::AllTracks);
        assert_eq!(src.mode, SourceMode::InOrder);
        assert_eq!(src.remaining + q.total, 54_515);
        let first = b.player_get_state().unwrap().track.unwrap().id;
        b.player_next().unwrap();
        let second = b.player_get_state().unwrap().track.unwrap().id;
        assert_ne!(first, second);
        assert_eq!(b.player_get_state().unwrap().status, PlaybackStatus::Playing);
        b.player_pause().unwrap();
        assert_eq!(b.player_get_state().unwrap().status, PlaybackStatus::Paused);
    }

    #[test]
    fn volume_is_db_tapered_and_bit_perfect_only_at_unity() {
        assert_eq!(volume_db(1.0), Some(0.0));
        assert_eq!(volume_db(0.0), None);
        assert!((volume_db(0.5).unwrap() + 30.0).abs() < 1e-9);
        let b = MockBackend::new();
        b.player_play_queue(PlayQueueRequest { track_ids: vec!["tr_0".into()], start_index: 0 }).unwrap();
        assert!(b.player_get_state().unwrap().bit_perfect);
        b.player_set_volume(SetVolumeRequest { volume: 0.5 }).unwrap();
        assert!(!b.player_get_state().unwrap().bit_perfect);
    }

    #[test]
    fn events_flow_and_spectrum_is_gated() {
        let b = MockBackend::new();
        let rx = b.subscribe();
        b.player_play_queue(PlayQueueRequest { track_ids: vec!["tr_1".into()], start_index: 0 }).unwrap();
        assert!(matches!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Event::PlayerState(_)));
        // spectrum off: no Spectrum event within 300 ms while playing
        std::thread::sleep(Duration::from_millis(300));
        assert!(!rx.try_iter().any(|e| matches!(e, Event::Spectrum(_))));
        b.player_set_spectrum_active(true).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(rx.try_iter().any(|e| matches!(e, Event::Spectrum(_))));
        b.player_set_spectrum_active(false).unwrap();
        b.player_pause().unwrap();
    }

    #[test]
    fn art_is_exact_size_and_deterministic() {
        let b = MockBackend::new();
        let a = b.art_get("al_5", 64).unwrap();
        assert_eq!((a.width, a.height, a.rgba.len()), (64, 64, 64 * 64 * 4));
        assert_eq!(a, b.art_get("al_5", 64).unwrap());
        assert_ne!(a.rgba, b.art_get("al_6", 64).unwrap().rgba);
    }
}
