//! The audio engine: one dedicated OS thread that owns the queue, decode
//! threads, ring buffers and output render thread, driven by a command
//! channel so the async `player_*`/`output_*` commands never touch
//! WASAPI/decoder state directly.
//!
//! Lazily started: [`EngineHandle::with_events`] does nothing but store its
//! inputs — the engine thread (and therefore any device/network work)
//! only spawns on the first command, so app startup does no audio work.
//!
//! Position is **render-side**: the render thread publishes how many real
//! frames the device has actually played (`RenderHandle::played_frames`,
//! i.e. frames written minus those still queued in the device buffer); the
//! engine adds the track's start offset (seek target) and subtracts the
//! frame index at which the track began in the current ring.
//!
//! Track changes are also render-side. When the decoder of the audible
//! track hits EOF it reports exactly how many frames it pushed, which marks
//! the boundary in the ring. The next track is opened immediately:
//! - same output format → its decoder is handed the *same* ring producer
//!   (true gapless; the render thread never notices), and the UI/queue
//!   switch to it only once the render side has played past the boundary;
//! - different format → the old ring is played out completely, then the
//!   device is reopened for the new format (an unavoidable short gap);
//! - end of queue → playback stops once the last frame has been heard.
//!
//! The next queued track is prefetched to disk (not RAM) as soon as the
//! current one starts (`TrackSource::open` begins its download at once).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rtrb::{Producer, RingBuffer};

use crate::ipc::{
    self, EnqueuePosition, OutputDevice, OutputMode, OutputStatus, PlaybackStatus, PlayerState,
    RepeatMode, SpectrumFrame, Track, TrackFormat,
};
use crate::library::{self, Db};

use super::autoplay::{self, AutoplaySource};
use super::decode::{self, DecodeCommand, DecodeEvent};
use super::format::{Container, NegotiatedFormat, SourceFormat};
use super::output::{self, RenderEvent, RenderHandle};
use super::prefs::Prefs;
use super::queue::{Queue, SourceStamp};
use super::replaygain::{self, ReplayGainMode};
use super::session::{PushKind, SaveRequest, SessionState, SessionStore};
use super::queue::SourcePools;
use super::source::TrackSource;
use super::abloop::AbLoop;
use crate::listen::{EndReason, ListenTracker, Sample, WriterMsg};
use super::spectrum::{peak_to_dbfs, SpectrumAnalyzer};

const STATE_TICK: Duration = Duration::from_millis(250); // 4 Hz player-state events
/// Engine poll interval while a track boundary is pending, so a format
/// change reopens the device promptly after the old track's last sample.
const BOUNDARY_TICK: Duration = Duration::from_millis(10);
const SPECTRUM_TICK: Duration = Duration::from_millis(16); // paced further inside SpectrumAnalyzer
/// Paused/stopped and unchanged: re-send only this often. The UI state
/// is fed by events alone, so a heartbeat still serves a view that
/// subscribed after the last change (reload, late mount) within a moment.
const IDLE_HEARTBEAT: Duration = Duration::from_secs(2);

/// Run-loop wait while idle (see `Engine::is_idle`): until the state
/// heartbeat is due (`heartbeat_age` = age of the last emitted state, `None`
/// = never emitted) or the debounced session write is (`persist_due` = time
/// left, `None` = nothing dirty), whichever is first; at most the heartbeat
/// interval, at least 20 ms so a deadline that rounds early cannot spin.
fn idle_wait(heartbeat_age: Option<Duration>, persist_due: Option<Duration>) -> Duration {
    let heartbeat_due = heartbeat_age.map_or(Duration::ZERO, |age| IDLE_HEARTBEAT.saturating_sub(age));
    let due = persist_due.map_or(heartbeat_due, |p| p.min(heartbeat_due));
    due.clamp(Duration::from_millis(20), IDLE_HEARTBEAT)
}

/// Whether this tick's state goes out: always while playing (position moves),
/// otherwise only when it differs from the last emitted one, or as the slow
/// heartbeat. `last` = the last emitted state and its age.
fn should_emit_state(last: Option<(&PlayerState, Duration)>, new: &PlayerState) -> bool {
    match last {
        Some((prev, age)) => new.status == PlaybackStatus::Playing || prev != new || age >= IDLE_HEARTBEAT,
        None => true,
    }
}

/// Consecutive dry polls (16 ms each) before the spectrum thread backs off:
/// ~200 ms without samples, so a single late pop while playing never stalls it.
const SPECTRUM_DRY_POLLS: u32 = 12;

/// Poll interval while the tap is dry (paused / stopped); the 250 ms tap ring
/// easily covers it, and the first popped samples switch back to the fast tick.
const SPECTRUM_IDLE_TICK: Duration = Duration::from_millis(100);
/// Poll interval while no view shows the spectrum (`Prefs::spectrum_active` off).
const SPECTRUM_INACTIVE_TICK: Duration = Duration::from_millis(250);
const FORMAT_TIMEOUT: Duration = Duration::from_secs(10);
/// Consecutive track failures (no track started in between) after which the
/// engine stops instead of skipping on: with repeat one/all, or offline with
/// a long queue, skipping would otherwise never end.
const MAX_CONSECUTIVE_FAILURES: u32 = 10;

pub enum EngineCommand {
    PlayQueue {
        track_ids: Vec<String>,
        start_index: u32,
    },
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
    Seek(f64),
    /// One press of the A-B loop button: set A at the audible position, then
    /// B, then clear (`audio::abloop`).
    LoopCycle,
    SetVolume(f32),
    /// A star/rating write succeeded for `id`: patch the cached `Track` so the
    /// next `player-state` doesn't revert the UI's optimistic heart/rating.
    /// `starred`/`rating` are `Some` only for the field that changed.
    UpdateTrackMeta {
        id: String,
        starred: Option<Option<String>>,
        rating: Option<Option<u8>>,
    },
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    /// "Spread artists in shuffle" setting ,
    /// mirrored onto `Queue::spread_artists`; takes effect on the next
    /// shuffle (toggling it doesn't itself reshuffle a queue already in
    /// progress).
    SetSpreadArtists(bool),
    /// "Queue window" setting,
    /// mirrored onto `Queue::set_window_size` — takes effect immediately
    /// (tops the window back up if it just grew).
    SetQueueWindowSize(u32),
    GetQueue {
        offset: u32,
        limit: u32,
        reply: tokio::sync::oneshot::Sender<ipc::QueueState>,
    },
    ReorderQueue {
        from_index: u32,
        to_index: u32,
    },
    RemoveFromQueue(u32),
    /// Start playing the queue entry at this play-order position (queue rail
    /// double-click / Enter). Out of range is ignored.
    JumpTo(u32),
    Enqueue {
        track_ids: Vec<String>,
        position: EnqueuePosition,
    },
    /// Starts a rolling-window `Source` (`audio::queue`'s module doc
    /// comment): `player_play_all` (Tracks route), playlist/artist "Play"/
    /// "Shuffle", and `radio_start` all land here. `track_ids` is already
    /// resolved from SQLite (or the server, for radio) in the caller's
    /// requested base order; `artists[i]` is `track_ids[i]`'s artist name
    /// (for the optional "spread artists" post-process), same length as
    /// `track_ids` (or empty to skip it). Unlike `PlayQueue`, metadata for
    /// these ids is resolved lazily, and only up to `queueWindowSize` of
    /// them are ever materialized in the queue at once — see the module doc
    /// comment; this is the path a 50k-track (or 1,980-track playlist)
    /// queue takes.
    PlaySource {
        kind: ipc::SourceKind,
        label: String,
        /// Playlist/artist id behind `label` (None for all-tracks/radio).
        source_id: Option<String>,
        mode: ipc::SourceMode,
        /// Flow only (`mode == Flow`): the energy arc. The engine loads the
        /// sequencing data for `track_ids` from the library itself.
        arc: ipc::FlowArc,
        track_ids: Vec<String>,
        artists: Vec<Option<String>>,
        start_track_id: Option<String>,
    },
    ListOutputDevices(tokio::sync::oneshot::Sender<Vec<OutputDevice>>),
    SetOutputDevice(String),
    SetOutputMode(OutputMode),
    GetOutputStatus(tokio::sync::oneshot::Sender<OutputStatus>),
    /// Snapshot of the current `PlayerState` (tests/examples; the UI gets
    /// the same struct via `player-state` events).
    GetPlayerState(tokio::sync::oneshot::Sender<PlayerState>),
    /// Full teardown: stop playback, drop the queue and cached track
    /// metadata, cancel any in-flight prefetch/probe. Sent on logout and on
    /// switching accounts (`auth_logout`/`auth_connect`) — the engine
    /// must not keep playing, or hold onto, ids that belonged to a different
    /// server/account.
    Stop,
    // --- continuity (session resume / ReplayGain) ---
    /// Re-applies the combined user-volume + ReplayGain gain to the open
    /// output (sent when the ReplayGain mode setting changes).
    RefreshGain,
    /// Restores a saved session (startup, `force = false`: ignored if the user
    /// already started playing) or a server play queue (`force = true`, an
    /// explicit "Resume from server" click: replaces the queue and plays).
    /// Ids missing from the local library are dropped. Never opens the stream
    /// unless `force`: the queue comes back paused at the saved position.
    RestoreSession {
        state: Box<SessionState>,
        pools: Option<SourcePools>,
        force: bool,
    },
    /// Final session write + server push at app exit; replies when the
    /// writer thread has finished (or immediately if persistence is off).
    PersistNow(std::sync::mpsc::Sender<()>),
    /// Replies once every listen session closed so far (incl. a preceding
    /// `Stop`'s) is in the DB: forwarded as a flush to the listen writer, so
    /// the engine thread never waits on SQLite. Dropped reply = no recorder.
    FlushListen(tokio::sync::oneshot::Sender<()>),
}

/// Where the engine's events go: the app's event sink in production, anything
/// else (a channel, a log) in examples/tests.
pub trait EngineEvents: Send + Sync + 'static {
    fn player_state(&self, state: &PlayerState);
    fn spectrum(&self, frame: SpectrumFrame);
}


// --- OS-integration hook ----------------------------------------------------
// Every `PlayerState` the engine emits to its event sink is *also* published
// on a `tokio::sync::watch` channel, so in-process consumers (`media::` for
// SMTC, `scrobble::`) can observe play/pause/track-change/position directly.
// This wraps `EngineEvents` rather than touching the engine's run loop or
// command handling.

/// Forwards every event to the real sink (the app in production, a test sink
/// in examples/tests) and mirrors `player_state` onto the watch channel.
struct BroadcastEvents {
    inner: Arc<dyn EngineEvents>,
    player_state_tx: tokio::sync::watch::Sender<PlayerState>,
}

impl EngineEvents for BroadcastEvents {
    fn player_state(&self, state: &PlayerState) {
        self.inner.player_state(state);
        // No receivers yet (e.g. media::/scrobble:: not started) is not an
        // error; the channel just has no subscribers.
        let _ = self.player_state_tx.send(state.clone());
    }
    fn spectrum(&self, frame: SpectrumFrame) {
        self.inner.spectrum(frame);
    }
}

/// Idle snapshot the watch channel starts with, before the engine thread
/// has ever run (so an early subscriber sees a well-formed "nothing loaded"
/// state instead of needing an `Option`).
fn idle_player_state() -> PlayerState {
    PlayerState {
        status: PlaybackStatus::Stopped,
        track: None,
        position_sec: 0.0,
        duration_sec: 0.0,
        output_mode: OutputMode::Shared,
        sample_rate: None,
        bit_depth: None,
        float_mix: false,
        format_label: None,
        volume: 1.0,
        bit_perfect: false,
        shuffle: false,
        repeat: RepeatMode::Off,
        queue_length: 0,
        error: None,
        volume_db: Some(0.0),
        replay_gain_db: None,
        loop_a: None,
        loop_b: None,
    }
}

/// One-line trace of the commands that replace or move the queue (with `SONIX_LOG` set the UI's
/// logger writes it, so a startup log names who started what). `None` for queries and high-frequency commands.
fn describe_queue_command(cmd: &EngineCommand) -> Option<String> {
    Some(match cmd {
        EngineCommand::PlaySource { kind, mode, track_ids, start_track_id, .. } => {
            format!("PlaySource kind={kind:?} mode={mode:?} n={} start={start_track_id:?}", track_ids.len())
        }
        EngineCommand::PlayQueue { track_ids, start_index } => format!("PlayQueue n={} start_index={start_index}", track_ids.len()),
        EngineCommand::RestoreSession { state, force, .. } => format!(
            "RestoreSession force={force} entries={} current={:?} position_ms={} volume={:?}",
            state.queue.entries.len(),
            state.queue.current,
            state.position_ms,
            state.volume
        ),
        EngineCommand::JumpTo(i) => format!("JumpTo {i}"),
        EngineCommand::Next => "Next".into(),
        EngineCommand::Previous => "Previous".into(),
        EngineCommand::Play => "Play".into(),
        EngineCommand::PlayPause => "PlayPause".into(),
        EngineCommand::Stop => "Stop".into(),
        EngineCommand::SetVolume(v) => format!("SetVolume {v}"),
        _ => return None,
    })
}

/// The handle the backend's player commands talk to. The actual
/// engine lives on its own thread once started.
pub struct EngineHandle {
    inner: Mutex<Option<Sender<EngineCommand>>>,
    events: Arc<dyn EngineEvents>,
    source: Arc<dyn TrackSource>,
    db: Arc<Db>,
    /// Observer hook; see [`Self::subscribe_player_state`].
    player_state_tx: tokio::sync::watch::Sender<PlayerState>,
    /// The persisted "spread artists in shuffle" setting at app startup
    /// (`secrets::Settings`), applied when the engine thread actually spawns
    /// (`sender()`) — stored here rather than sent as a command so reading
    /// it back doesn't itself trigger the lazy engine-thread spawn this
    /// struct otherwise avoids (see the module doc comment). Later toggles
    /// go straight to the live engine via `EngineCommand::SetSpreadArtists`.
    initial_spread_artists: bool,
    /// Same deal as `initial_spread_artists`, for the persisted "queue
    /// window" setting (`Queue::window_size`); later changes go straight to
    /// the live engine via `EngineCommand::SetQueueWindowSize`.
    initial_queue_window_size: u32,
    // --- continuity: live prefs, session persistence, autoplay provider ---
    prefs: Arc<Prefs>,
    session: Option<Arc<SessionStore>>,
    autoplay_source: Option<Arc<dyn AutoplaySource>>,
    /// Finished listen sessions go here (`listen::spawn_writer`).
    listen_tx: Option<tokio::sync::mpsc::UnboundedSender<WriterMsg>>,
}

impl EngineHandle {
    /// Engine wired to an arbitrary event sink (examples/tests).
    pub fn with_events(
        events: Arc<dyn EngineEvents>,
        source: Arc<dyn TrackSource>,
        db: Arc<Db>,
        initial_spread_artists: bool,
        initial_queue_window_size: u32,
    ) -> Self {
        let (player_state_tx, _rx) = tokio::sync::watch::channel(idle_player_state());
        let broadcast: Arc<dyn EngineEvents> = Arc::new(BroadcastEvents {
            inner: events,
            player_state_tx: player_state_tx.clone(),
        });
        Self {
            inner: Mutex::new(None),
            events: broadcast,
            source,
            db,
            player_state_tx,
            initial_spread_artists,
            initial_queue_window_size,
            prefs: Arc::new(Prefs::default()),
            session: None,
            autoplay_source: None,
            listen_tx: None,
        }
    }

    /// Wires the listen-event recorder (builder; call before the first
    /// command). Without it nothing is recorded.
    pub fn with_listen(mut self, tx: tokio::sync::mpsc::UnboundedSender<WriterMsg>) -> Self {
        self.listen_tx = Some(tx);
        self
    }

    /// Wires the continuity features (builder; call before the first command):
    /// the live prefs (autoplay / ReplayGain mode), the session store for
    /// persistence + server queue sync, and the autoplay similar-tracks
    /// provider. Without it (examples, most tests) the engine runs with
    /// defaults and persists/autoplays nothing.
    pub fn with_continuity(
        mut self,
        prefs: Arc<Prefs>,
        session: Option<Arc<SessionStore>>,
        autoplay_source: Option<Arc<dyn AutoplaySource>>,
    ) -> Self {
        self.prefs = prefs;
        self.session = session;
        self.autoplay_source = autoplay_source;
        self
    }

    /// Whether the engine thread exists (it is spawned lazily): exit-time
    /// persistence must not spawn it just to find nothing to save.
    pub fn is_started(&self) -> bool {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).is_some()
    }

    /// Exit path: final session write + `savePlayQueue`, bounded by `timeout`.
    pub fn persist_now(&self, timeout: Duration) {
        if !self.is_started() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.send(EngineCommand::PersistNow(tx));
        let _ = rx.recv_timeout(timeout);
    }

    /// Logout / account switch: stops the engine and waits (bounded) until
    /// the listen session that `Stop` closed, and any still queued before
    /// it, are written, so `Db::clear_all` afterwards wipes them with the
    /// old account instead of them landing in the next one.
    pub async fn stop_and_flush_listen(&self, timeout: Duration) {
        self.send(EngineCommand::Stop);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(EngineCommand::FlushListen(tx));
        let _ = tokio::time::timeout(timeout, rx).await;
    }

    /// In-process observer hook (`media::`/`scrobble::`): every
    /// `PlayerState` the engine produces, without going through the UI
    /// event sink. `watch` semantics: a lagging subscriber only ever sees the
    /// latest value, never a backlog.
    /// The last emitted `PlayerState` (idle before the engine ever ran), read
    /// from the watch channel: no engine round-trip, never spawns the thread.
    pub fn last_player_state(&self) -> PlayerState {
        self.player_state_tx.borrow().clone()
    }

    pub fn subscribe_player_state(&self) -> tokio::sync::watch::Receiver<PlayerState> {
        self.player_state_tx.subscribe()
    }

    fn sender(&self) -> Sender<EngineCommand> {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(tx) = guard.as_ref() {
            return tx.clone();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let events = self.events.clone();
        let source = self.source.clone();
        let db = self.db.clone();
        let spread_artists = self.initial_spread_artists;
        let queue_window_size = self.initial_queue_window_size;
        let prefs = self.prefs.clone();
        let session = self.session.clone();
        let autoplay_source = self.autoplay_source.clone();
        let listen_tx = self.listen_tx.clone();
        std::thread::Builder::new()
            .name("sonix-engine".into())
            .spawn(move || {
                let mut engine = Engine::new(events, source, db, spread_artists, queue_window_size);
                engine.prefs = prefs;
                engine.session = session;
                engine.autoplay_source = autoplay_source;
                engine.listen_tx = listen_tx;
                engine.run(rx)
            })
            .expect("spawn audio engine thread");
        *guard = Some(tx.clone());
        tx
    }

    pub fn send(&self, cmd: EngineCommand) {
        let _ = self.sender().send(cmd);
    }

    pub async fn request<T>(
        &self,
        make: impl FnOnce(tokio::sync::oneshot::Sender<T>) -> EngineCommand,
    ) -> Option<T> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(make(tx));
        rx.await.ok()
    }

    /// Blocking variant of [`Self::request`] for non-async callers.
    pub fn request_blocking<T>(
        &self,
        make: impl FnOnce(tokio::sync::oneshot::Sender<T>) -> EngineCommand,
    ) -> Option<T> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(make(tx));
        rx.blocking_recv().ok()
    }

    /// Blocking request for side readers (the MCP bridge) that must neither
    /// start the engine thread nor wait on a stalled one: `NotRunning` when
    /// the engine was never started (nothing queued, nothing to read),
    /// `NoAnswer` when it doesn't reply within `timeout`.
    pub fn request_blocking_if_running<T>(
        &self,
        make: impl FnOnce(tokio::sync::oneshot::Sender<T>) -> EngineCommand,
        timeout: Duration,
    ) -> Result<T, EngineRequestError> {
        let sender = {
            let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            guard.as_ref().cloned().ok_or(EngineRequestError::NotRunning)?
        };
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        sender.send(make(tx)).map_err(|_| EngineRequestError::NoAnswer)?;
        let deadline = Instant::now() + timeout;
        loop {
            match rx.try_recv() {
                Ok(v) => return Ok(v),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => return Err(EngineRequestError::NoAnswer),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    if Instant::now() >= deadline {
                        return Err(EngineRequestError::NoAnswer);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    /// Whether the engine thread has been started (tests).
    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).is_some()
    }
}

/// Why [`EngineHandle::request_blocking_if_running`] has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineRequestError {
    NotRunning,
    NoAnswer,
}

/// (endpoint id it was resolved for, resolved name).
type DeviceNameEntry = (String, Option<String>);

/// The decode thread currently feeding the ring.
struct Feeder {
    commands: Sender<DecodeCommand>,
    events: Receiver<DecodeEvent>,
}

impl Feeder {
    fn stop(&self) {
        let _ = self.commands.send(DecodeCommand::Stop);
    }
}

/// A track as positioned in the current ring.
#[derive(Clone)]
struct Slot {
    id: String,
    format: SourceFormat,
    /// Ring frame index (in `RenderHandle::played_frames` units) at which
    /// this track's first frame plays.
    start_frame: u64,
    /// Track time (seconds) of that first frame (non-zero after a seek).
    start_offset: f64,
}

/// A track boundary the render side hasn't reached yet.
struct Pending {
    /// `played_frames` value at which the audible track ends.
    boundary: u64,
    /// The track that follows (None: end of queue).
    next: Option<Slot>,
    /// `Some(decoder)` when the next track needs a different output format:
    /// its probed decoder waits for `Begin` until the device is reopened.
    reopen: Option<Feeder>,
    /// The queue as it will be once the boundary is crossed (computed on a
    /// clone at EOF, so `Next`/`Previous` during the ring's tail still act
    /// on the track that's audible). `None` if the queue was edited since.
    queue_after: Option<Queue>,
}

struct Playback {
    negotiated: NegotiatedFormat,
    render: RenderHandle,
    render_events: Receiver<RenderEvent>,
    spectrum_stop: Arc<AtomicBool>,
    device_name: Option<String>,
    /// The default render endpoint at open time when following the system
    /// default (`output_device_id == None`); `None` when a device was chosen
    /// or the lookup failed. A skip reuses the output only while it still matches.
    opened_default: Option<String>,
    current: Slot,
    feeder: Option<Feeder>,
    pending: Option<Pending>,
    /// In-place seeks handed to the render thread (`RenderHandle::seeks_applied`).
    seeks_sent: u64,
}

impl Playback {
    fn played_frames(&self) -> u64 {
        self.render.played_frames.load(Ordering::Relaxed)
    }

    fn position_sec(&self) -> f64 {
        // A seek the render thread has not picked up yet: its frame counter
        // still belongs to the old ring, so the position is the seek target.
        if self.render.seeks_applied.load(Ordering::Acquire) < self.seeks_sent {
            return self.current.start_offset;
        }
        let frames = self
            .played_frames()
            .saturating_sub(self.current.start_frame);
        self.current.start_offset + frames as f64 / self.negotiated.sample_rate.max(1) as f64
    }

    fn shutdown(self) {
        if let Some(f) = &self.feeder {
            f.stop();
        }
        if let Some(Pending {
            reopen: Some(f), ..
        }) = &self.pending
        {
            f.stop();
        }
        self.spectrum_stop.store(true, Ordering::Relaxed);
        self.render.stop_and_join();
    }
}

/// A decode thread spawned for the track the engine wants to (re)start,
/// whose probed format hasn't arrived yet. `TrackSource::open` (called
/// before this is constructed) never blocks — it only opens a local temp
/// file or starts a background download — but the probe itself can take a
/// while on a stalled/slow HTTP connection, so it's polled from `pump()`
/// (`Engine::poll_pending_start`) instead of waited on synchronously inside
/// a command handler: command handling (pause/next/...) is never blocked by
/// a slow network probe, bounded by `FORMAT_TIMEOUT`.
struct PendingStart {
    id: String,
    commands: Sender<DecodeCommand>,
    events: Receiver<DecodeEvent>,
    started: Instant,
    /// Seconds to seek into the track once the decoder is ready (`None`:
    /// start from 0) — updated in place if `Seek` arrives while still
    /// loading.
    at: Option<f64>,
    /// Whether playback should begin paused — set/cleared by `Pause`/`Play`
    /// arriving while still loading.
    start_paused: bool,
}

impl PendingStart {
    fn stop(&self) {
        let _ = self.commands.send(DecodeCommand::Stop);
    }
}

/// Output side of a freshly opened stream.
struct OpenedOutput {
    negotiated: NegotiatedFormat,
    producer: Producer<u8>,
    render: RenderHandle,
    render_events: Receiver<RenderEvent>,
    spectrum_stop: Arc<AtomicBool>,
}

struct Engine {
    events: Arc<dyn EngineEvents>,
    source: Arc<dyn TrackSource>,
    db: Arc<Db>,
    queue: Queue,
    /// Real metadata (title/artist/album/cover/duration/format) for every id
    /// still in the queue (`PlayQueue`/`PlaySource` drop the rest, and
    /// `bound_track_cache` trims ids a rolling window has pruned), resolved from the local
    /// library DB — populated once per id (`resolve_tracks`, called from
    /// `PlayQueue`/`Enqueue`, never from the render/state-tick hot path) and
    /// never fabricated: an id with no local row just has no cached entry
    /// and falls back to the light, honestly-empty stub in `track_for`.
    track_cache: HashMap<String, Track>,
    volume: f32,
    output_device_id: Option<String>,
    /// Resolved name of the selected (or current default) endpoint, with its
    /// id: `output::list_devices()` enumerates every endpoint over COM on a
    /// fresh thread, too heavy for each track start.
    device_name_cache: std::cell::RefCell<Option<DeviceNameEntry>>,
    output_mode_pref: OutputMode,
    /// Default render endpoint lookup (a seam for tests).
    default_id_lookup: fn() -> Option<String>,
    status: PlaybackStatus,
    playback: Option<Playback>,
    /// Decoder spawned for a track that hasn't reported its format yet; see
    /// `PendingStart`'s doc comment.
    pending_start: Option<PendingStart>,
    /// The output of the track a user skip just silenced (see `park_for_skip`):
    /// device + render thread stay open, fed an empty ring, while the next
    /// track's probe is in flight. `begin_playback` reuses it when the new
    /// format fits (`can_switch_in_place`), otherwise shuts it down and opens
    /// the output as before. Only ever `Some` while `pending_start` is.
    parked: Option<Playback>,
    prefetched: Option<(String, Box<dyn symphonia::core::io::MediaSource>)>,
    /// Why exclusive mode isn't in use right now (named HRESULT), if it was
    /// requested and refused; surfaced as `OutputStatus.exclusiveError`.
    exclusive_error: Option<String>,
    /// Where to resume after the device was lost / output restarted.
    resume_at: Option<f64>,
    /// Most recent playback failure (stream error, probe timeout, device
    /// open failure, ...), surfaced via `PlayerState.error`. Cleared at the
    /// start of every new `start_current`.
    last_error: Option<String>,
    /// See `MAX_CONSECUTIVE_FAILURES`; reset when a track starts or the user
    /// picks something to play.
    consecutive_failures: u32,
    /// Set by the first command that changes the queue (play, enqueue, remove, reorder, jump,
    /// skip, stop, a forced restore). A startup (non-forced) restore is refused from then on, even when the queue
    /// is empty again: the user emptied it on purpose, and a re-offered saved session must not bring it back.
    queue_touched: bool,
    /// Set by a failure reported *while* `fail_current_track` is already
    /// skipping (a synchronous open failure inside its `advance`), so the
    /// skip continues in its loop instead of recursing (deep queues used to
    /// recurse once per failing track).
    skip_pending: bool,
    in_failure_chain: bool,
    last_emit: Instant,
    /// The last state actually emitted, and when (see `should_emit_state`).
    last_emitted: Option<(PlayerState, Instant)>,
    // --- continuity ---
    prefs: Arc<Prefs>,
    session: Option<Arc<SessionStore>>,
    autoplay_source: Option<Arc<dyn AutoplaySource>>,
    /// Listen-event recorder (`listen::`): fed every state sample, told why
    /// the audible entry changes; closed sessions go out on `listen_tx`.
    listen: ListenTracker,
    listen_tx: Option<tokio::sync::mpsc::UnboundedSender<WriterMsg>>,
    autoplay: AutoplayState,
    /// Most recently audible track ids (oldest first, `RECENT_LIMIT`), so
    /// autoplay never queues something that just played.
    recent_played: std::collections::VecDeque<String>,
    last_audible: Option<String>,
    tracking: SessionTracking,
    /// A-B loop markers and the queue entry they belong to (cleared when the
    /// current track changes).
    ab_loop: AbLoop,
    loop_track: Option<String>,
}

/// An autoplay similar-tracks lookup running on a helper thread.
struct InflightAutoplay {
    seed: String,
    rx: Receiver<Vec<(String, Option<String>)>>,
}

#[derive(Default)]
struct AutoplayState {
    inflight: Option<InflightAutoplay>,
    /// The track a lookup was already made for; an empty answer for it is not
    /// retried (avoids a request loop on a dead-end radio).
    requested_for: Option<String>,
    /// The queue ran out (EOF or Next on the last track) while the lookup was
    /// still running: start the radio as soon as it answers.
    start_on_arrival: bool,
}

/// What the session writer has been told about, to decide when to write again.
struct SessionTracking {
    /// Set once the queue holds something the user chose (or a session was
    /// restored): an idle engine must never overwrite the saved session.
    enabled: bool,
    /// The queue changed since the last write that carried it.
    queue_dirty: bool,
    /// The writer has no core yet (first write after start/restore/clear).
    need_core: bool,
    last_write: Instant,
    last_stamp: Option<SourceStamp>,
    last_current: Option<String>,
    last_status: PlaybackStatus,
    /// Something was written (i.e. the user did something) since startup or
    /// the last restore; exit only pushes to the server if so, so merely
    /// opening and closing the app never overwrites a newer server queue.
    activity: bool,
}

/// Queue/state writes are coalesced to at most one per this interval...
const SESSION_WRITE_DEBOUNCE: Duration = Duration::from_millis(1500);
/// ...and position is refreshed this often while playing.
const SESSION_POSITION_INTERVAL: Duration = Duration::from_secs(10);
/// Most recent audible ids remembered for autoplay's exclusion list.
const RECENT_PLAYED_LIMIT: usize = autoplay::RECENT_LIMIT;

impl Engine {
    fn new(
        events: Arc<dyn EngineEvents>,
        source: Arc<dyn TrackSource>,
        db: Arc<Db>,
        spread_artists_in_shuffle: bool,
        queue_window_size: u32,
    ) -> Self {
        let mut queue = Queue::new();
        queue.spread_artists = spread_artists_in_shuffle;
        queue.set_window_size(queue_window_size as usize);
        Self {
            events,
            source,
            db,
            queue,
            track_cache: HashMap::new(),
            volume: 1.0,
            output_device_id: None,
            device_name_cache: std::cell::RefCell::new(None),
            output_mode_pref: OutputMode::Exclusive,
            default_id_lookup: real_default_device_id,
            status: PlaybackStatus::Stopped,
            playback: None,
            pending_start: None,
            parked: None,
            prefetched: None,
            exclusive_error: None,
            resume_at: None,
            last_error: None,
            consecutive_failures: 0,
            queue_touched: false,
            skip_pending: false,
            in_failure_chain: false,
            last_emit: Instant::now(),
            last_emitted: None,
            prefs: Arc::new(Prefs::default()),
            session: None,
            autoplay_source: None,
            listen: ListenTracker::default(),
            listen_tx: None,
            autoplay: AutoplayState::default(),
            recent_played: std::collections::VecDeque::new(),
            last_audible: None,
            ab_loop: AbLoop::default(),
            loop_track: None,
            tracking: SessionTracking {
                enabled: false,
                queue_dirty: false,
                need_core: true,
                last_write: Instant::now(),
                last_stamp: None,
                last_current: None,
                last_status: PlaybackStatus::Stopped,
                activity: false,
            },
        }
    }

    fn run(mut self, rx: Receiver<EngineCommand>) {
        loop {
            let tick = if self.ab_loop.is_active() && self.status == PlaybackStatus::Playing {
                // Hit B within ~10 ms of the device reaching it.
                BOUNDARY_TICK
            } else if self.pending_start.is_some()
                || self
                    .playback
                    .as_ref()
                    .is_some_and(|pb| pb.pending.is_some() && !pb.render.paused.load(Ordering::Relaxed))
            {
                BOUNDARY_TICK
            } else if self.is_idle() {
                // Nothing to poll: sleep until the next deadline (state
                // heartbeat, debounced session write) or a command.
                let persist_due = (self.session.is_some() && self.tracking.enabled && self.tracking.queue_dirty)
                    .then(|| SESSION_WRITE_DEBOUNCE.saturating_sub(self.tracking.last_write.elapsed()));
                idle_wait(self.last_emitted.as_ref().map(|(_, at)| at.elapsed()), persist_due)
            } else {
                STATE_TICK
            };
            match rx.recv_timeout(tick) {
                Ok(cmd) => {
                    let mutates = !matches!(
                        cmd,
                        EngineCommand::GetQueue { .. }
                            | EngineCommand::GetPlayerState(_)
                            | EngineCommand::GetOutputStatus(_)
                            | EngineCommand::ListOutputDevices(_)
                            | EngineCommand::PersistNow(_)
                            | EngineCommand::FlushListen(_)
                            | EngineCommand::LoopCycle
                            | EngineCommand::RestoreSession { force: false, .. }
                    );
                    if log::log_enabled!(log::Level::Info)
                        && let Some(what) = describe_queue_command(&cmd) {
                            log::info!("engine: {what}");
                        }
                    self.handle(cmd);
                    if mutates {
                        self.tracking.queue_dirty = true;
                    }
                    // Commands change state the UI should see right away.
                    self.last_emit = Instant::now() - STATE_TICK;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.teardown_playback();
                    return;
                }
            }
            self.pump();
            self.continuity_tick();
            if self.last_emit.elapsed() >= STATE_TICK {
                self.last_emit = Instant::now();
                let state = self.player_state();
                self.listen_observe(&state);
                let since = self.last_emitted.as_ref().map(|(s, at)| (s, at.elapsed()));
                if should_emit_state(since, &state) {
                    self.events.player_state(&state);
                    self.last_emitted = Some((state, Instant::now()));
                }
            }
        }
    }

    /// Stopped/paused with nothing in flight that `pump()`/`continuity_tick()`
    /// would have to poll for: no probe, parked output, autoplay lookup or
    /// answer wait, gapless hand-off or A-B loop. (Render-thread device-lost
    /// and decoder EOF events are still read, just at the idle wake-ups: the
    /// state is already paused/stopped, so noticing them <= 2 s late is
    /// harmless; any command wakes the loop at once.)
    fn is_idle(&self) -> bool {
        matches!(self.status, PlaybackStatus::Stopped | PlaybackStatus::Paused)
            && self.pending_start.is_none()
            && self.parked.is_none()
            && self.autoplay.inflight.is_none()
            && !self.autoplay.start_on_arrival
            && !self.ab_loop.is_active()
            && self.playback.as_ref().is_none_or(|pb| pb.pending.is_none())
    }

    // --- listen events (`listen::`) --------------------------------------

    /// Feeds the recorder the state as it is right now. Called before a
    /// command mutates the audible entry, so the session being ended has
    /// its latest position and the reason (noted next) applies to it.
    fn listen_sample(&mut self) {
        let state = self.player_state();
        self.listen_observe(&state);
    }

    fn listen_observe(&mut self, state: &PlayerState) {
        let sample = Sample {
            track_id: state.track.as_ref().map(|t| t.id.as_str()),
            status: state.status,
            position_sec: state.position_sec,
            duration_sec: state.duration_sec,
            entry_origin: self.queue.current_origin(),
            source: self.queue.source_ref(),
        };
        let closed = self.listen.observe(&sample, chrono::Utc::now().timestamp_millis(), self.prefs.play_threshold_sec());
        if let (Some(s), Some(tx)) = (closed, &self.listen_tx) {
            let _ = tx.send(WriterMsg::Session(s));
        }
    }

    /// Ends the open session (Stop / app exit). `sync`: write it right now
    /// on this thread (exit only: the process is about to end, so the async
    /// writer may never run). That insert waits on the DB writer *mutex*
    /// (not bounded by SQLite's busy timeout), so the exit path calls it
    /// after the session file is saved, inside `persist_now`'s 4 s bound.
    /// Otherwise (Stop) it goes to the writer like any other session; logout
    /// waits for it with `FlushListen`.
    fn listen_close(&mut self, reason: EndReason, sync: bool) {
        let Some(tx) = &self.listen_tx else { return };
        let tx = tx.clone();
        self.listen_sample();
        let thr = self.prefs.play_threshold_sec();
        let Some(s) = self.listen.finish(reason, chrono::Utc::now().timestamp_millis(), thr) else { return };
        if !sync {
            let _ = tx.send(WriterMsg::Session(s));
        } else if let Err(e) = crate::listen::insert_session(&self.db, &s) {
            log::warn!("failed to record listen event: {}", e.message);
        }
    }

    fn handle(&mut self, cmd: EngineCommand) {
        if matches!(
            cmd,
            EngineCommand::PlayQueue { .. }
                | EngineCommand::PlaySource { .. }
                | EngineCommand::Enqueue { .. }
                | EngineCommand::RemoveFromQueue(_)
                | EngineCommand::ReorderQueue { .. }
                | EngineCommand::JumpTo(_)
                | EngineCommand::Next
                | EngineCommand::Previous
                | EngineCommand::Stop
                | EngineCommand::RestoreSession { force: true, .. }
        ) {
            self.queue_touched = true;
        }
        match cmd {
            EngineCommand::PlayQueue {
                track_ids,
                start_index,
            } => {
                self.listen_sample();
                self.listen.note(EndReason::Jump);
                self.consecutive_failures = 0;
                self.teardown_playback();
                // A new queue replaces the old one entirely: drop metadata
                // for ids that are no longer queued, otherwise the cache
                // grows by every album/playlist played this session.
                let keep: std::collections::HashSet<&str> = track_ids.iter().map(String::as_str).collect();
                self.track_cache.retain(|id, _| keep.contains(id.as_str()));
                self.resolve_tracks(&track_ids);
                self.queue.set_queue(track_ids, start_index as usize);
                self.reset_autoplay();
                self.start_current(None);
            }
            EngineCommand::PlayPause => match self.status {
                PlaybackStatus::Playing => self.pause(),
                // Waiting for a late autoplay answer: the toggle cancels it.
                PlaybackStatus::Loading if self.autoplay.start_on_arrival => self.pause(),
                _ => self.resume_or_start(),
            },
            EngineCommand::Play => self.resume_or_start(),
            EngineCommand::Pause => self.pause(),
            EngineCommand::Next => {
                self.listen_sample();
                self.listen.note(EndReason::Next);
                self.consecutive_failures = 0;
                self.advance(true)
            }
            EngineCommand::Previous => {
                if self.position_sec() > 3.0 {
                    self.seek(0.0);
                } else {
                    self.listen_sample();
                    self.listen.note(EndReason::Previous);
                    self.advance(false);
                }
            }
            EngineCommand::Seek(sec) => self.seek(sec),
            EngineCommand::LoopCycle => self.loop_cycle(),
            EngineCommand::UpdateTrackMeta { id, starred, rating } => {
                if let Some(t) = self.track_cache.get_mut(&id) {
                    if let Some(s) = starred {
                        t.starred = s;
                    }
                    if let Some(r) = rating {
                        t.rating = r;
                    }
                }
            }
            EngineCommand::SetVolume(v) => {
                // `v` is the slider position; the render thread gets the
                // tapered linear gain (`output::volume_to_gain`, exactly 1.0
                // at the top so bit-perfect output is untouched).
                self.volume = if v.is_finite() { v.clamp(0.0, 1.0) } else { 1.0 };
                // Applied at render time (`output::apply_gain_in_place`),
                // not sent to the decoder: audible within one render
                // callback instead of after the ~1.5 s ring buffer drains.
                // (Combined with the audible track's ReplayGain, if enabled.)
                self.apply_gain();
            }
            EngineCommand::JumpTo(pos) => {
                self.listen_sample();
                if self.queue.jump_to(pos as usize) {
                    self.listen.note(EndReason::Jump);
                    self.consecutive_failures = 0;
                    self.park_for_skip();
                    self.start_current(None);
                }
            }
            EngineCommand::SetShuffle(enabled) => {
                self.queue.set_shuffle(enabled);
                self.queue_edited();
            }
            EngineCommand::SetRepeat(mode) => {
                self.queue.set_repeat(mode);
                self.queue_edited();
            }
            // Both settings live on `Queue`, so a pending gapless boundary's
            // `queue_after` snapshot (cloned at the previous track's EOF)
            // would silently revert them when it's committed: drop it, the
            // boundary then advances the live queue instead.
            EngineCommand::SetSpreadArtists(enabled) => {
                self.queue.spread_artists = enabled;
                self.queue_edited();
            }
            EngineCommand::SetQueueWindowSize(size) => {
                self.queue.set_window_size(size as usize);
                self.queue_edited();
            }
            EngineCommand::GetQueue { offset, limit, reply } => {
                // Only this page's ids are resolved from the library DB —
                // never the whole queue (see the module doc comment and
                // `Engine::resolve_tracks`), so paging a 50k-entry "shuffle
                // all" queue (or a rolling window drawn from one) stays
                // cheap no matter how deep the user scrolls.
                let page = self.queue.page(offset as usize, limit as usize);
                let page_ids: Vec<String> = page.iter().map(|(id, _)| id.clone()).collect();
                self.resolve_tracks(&page_ids);
                let items = page
                    .into_iter()
                    .enumerate()
                    .map(|(i, (id, origin))| ipc::QueueItem {
                        track: self.track_for(&id),
                        origin,
                        flow: self.queue.flow_reason(offset as usize + i),
                    })
                    .collect();
                let flow_info = self.queue.flow_info();
                let source = self.queue.source_info().map(|(kind, label, mode, remaining, id)| ipc::QueueSourceInfo {
                    kind,
                    label,
                    id,
                    mode,
                    remaining,
                    arc: flow_info.map(|f| f.0),
                    flow_coverage: flow_info.map(|f| f.1),
                });
                let _ = reply.send(ipc::QueueState {
                    items,
                    total: self.queue.len() as u64,
                    current_index: self
                        .queue
                        .current_position()
                        .map(|p| p as i64)
                        .unwrap_or(-1),
                    source,
                });
            }
            EngineCommand::ReorderQueue {
                from_index,
                to_index,
            } => {
                self.queue.reorder(from_index as usize, to_index as usize);
                self.queue_edited();
            }
            EngineCommand::RemoveFromQueue(index) => {
                let was_current = self.queue.current_position() == Some(index as usize);
                if was_current {
                    self.listen_sample();
                    self.listen.note(EndReason::Next);
                }
                self.queue.remove(index as usize);
                self.queue_edited();
                if was_current {
                    self.park_for_skip();
                    self.start_current(None);
                }
            }
            EngineCommand::Enqueue {
                track_ids,
                position,
            } => {
                self.resolve_tracks(&track_ids);
                match position {
                    EnqueuePosition::Next => self.queue.enqueue_next(track_ids),
                    EnqueuePosition::End => self.queue.enqueue_end(track_ids),
                }
                self.queue_edited();
            }
            EngineCommand::PlaySource {
                kind,
                label,
                source_id,
                mode,
                arc,
                track_ids,
                artists,
                start_track_id,
            } => {
                self.listen_sample();
                self.listen.note(EndReason::Jump);
                self.consecutive_failures = 0;
                self.teardown_playback();
                // A fresh source: bound `track_cache` to what the queue
                // actually needs (current/next + whatever `GetQueue` pages
                // in, which with a rolling window never grows past a couple
                // of window's worth of ids) rather than letting metadata
                // from a previous session's queue(s) linger indefinitely.
                self.track_cache.clear();
                if mode == ipc::SourceMode::Flow {
                    // One batched read of the pool's features (tens of ms
                    // at most, even for the whole library) before drawing.
                    let features = library::flow::load_flow_features(&self.db, &track_ids).unwrap_or_else(|e| {
                        log::warn!("flow: feature read failed, sequencing blind: {}", e.message);
                        Default::default()
                    });
                    self.queue.set_source_flow(kind, label, arc, track_ids, features, start_track_id.as_deref());
                } else {
                    self.queue.set_source(kind, label, mode, track_ids, artists, start_track_id.as_deref());
                }
                self.queue.set_source_id(source_id);
                self.reset_autoplay();
                self.start_current(None);
            }
            EngineCommand::ListOutputDevices(reply) => {
                self.device_name_cache.take(); // the user is looking at the list: re-resolve next time
                let devices = output::list_devices()
                    .into_iter()
                    .map(|d| OutputDevice {
                        id: d.id,
                        name: d.name,
                        is_default: d.is_default,
                    })
                    .collect();
                let _ = reply.send(devices);
            }
            EngineCommand::SetOutputDevice(id) => {
                if self.output_device_id.as_deref() == Some(id.as_str()) && self.exclusive_error.is_none() {
                    return; // unchanged: no reopen, no restart, no re-download
                }
                self.output_device_id = Some(id);
                self.device_name_cache.take();
                self.restart_output();
            }
            EngineCommand::SetOutputMode(mode) => {
                // Re-selecting the mode that is already in effect is a no-op
                // (it used to tear down playback and re-fetch the stream). A
                // refused exclusive request still retries on a second press.
                let in_effect = self.playback.as_ref().is_none_or(|pb| pb.negotiated.mode == mode);
                if mode == self.output_mode_pref && in_effect {
                    return;
                }
                self.output_mode_pref = mode;
                if mode == OutputMode::Shared {
                    // Nothing exclusive is requested any more: a refusal from
                    // the previous attempt must not linger until (or, with
                    // nothing playing, beyond) the next open, or Settings
                    // keeps showing it and snaps the toggle back to Exclusive.
                    self.exclusive_error = None;
                }
                self.restart_output();
            }
            EngineCommand::GetOutputStatus(reply) => {
                let _ = reply.send(self.output_status());
            }
            EngineCommand::GetPlayerState(reply) => {
                let _ = reply.send(self.player_state());
            }
            EngineCommand::Stop => {
                // Through the writer: logout / account switch follow up with
                // `FlushListen` before wiping, so the row is never orphaned
                // in the next account, and this handler never waits on SQLite.
                self.listen_close(EndReason::Stopped, false);
                self.teardown_playback();
                self.cancel_pending_start();
                self.queue = Queue::new();
                self.track_cache.clear();
                self.prefetched = None;
                self.resume_at = None;
                self.last_error = None;
                self.status = PlaybackStatus::Stopped;
                // Logout / account switch: nothing of the old session may
                // linger (no autoplay, no saved ids of the other account).
                self.reset_autoplay();
                self.recent_played.clear();
                self.tracking.enabled = false;
                self.tracking.activity = false;
                self.tracking.need_core = true;
                self.tracking.last_stamp = None;
                self.tracking.last_current = None;
                if let Some(s) = &self.session {
                    s.clear();
                }
            }
            EngineCommand::RefreshGain => self.apply_gain(),
            EngineCommand::RestoreSession { state, pools, force } => self.restore_session(*state, pools, force),
            EngineCommand::PersistNow(reply) => {
                // Session file first: the listen insert may wait on the DB
                // writer mutex (a sync transaction), and must never cost the
                // resume session `persist_now`'s 4 s bound.
                self.persist_final();
                self.listen_close(EndReason::Quit, true);
                let _ = reply.send(());
            }
            EngineCommand::FlushListen(reply) => match &self.listen_tx {
                Some(tx) => {
                    let _ = tx.send(WriterMsg::Flush(reply));
                }
                None => {
                    let _ = reply.send(());
                }
            },
        }
    }

    // --- Transport -------------------------------------------------------

    fn position_sec(&self) -> f64 {
        match &self.playback {
            Some(pb) => pb.position_sec(),
            None => self.resume_at.unwrap_or(0.0),
        }
    }

    fn resume_or_start(&mut self) {
        if let Some(pb) = &self.playback {
            pb.render.set_paused(false);
            self.status = PlaybackStatus::Playing;
        } else if let Some(p) = &mut self.pending_start {
            // Still probing/opening the device: just clear the "start
            // paused" intent, no decoder/output exists yet to resume.
            p.start_paused = false;
            self.status = PlaybackStatus::Loading;
        } else if self.queue.current_id().is_some() {
            let at = self.resume_at.take();
            self.start_current(at);
        }
    }

    /// Pausing writes silence from the render thread without popping the
    /// ring (see `output::RenderHandle::paused`) — decode naturally blocks
    /// on the still-full ring, so there's nothing else to coordinate here.
    fn pause(&mut self) {
        if let Some(pb) = &self.playback {
            pb.render.set_paused(true);
            self.status = PlaybackStatus::Paused;
        } else if let Some(p) = &mut self.pending_start {
            p.start_paused = true;
            self.status = PlaybackStatus::Paused;
        } else if self.autoplay.start_on_arrival {
            // The queue ran out and autoplay's answer is still pending:
            // pausing means "don't start anything": the late answer is then
            // dropped and the queue simply stays ended.
            self.autoplay.start_on_arrival = false;
            self.status = PlaybackStatus::Stopped;
        }
    }

    fn advance(&mut self, forward: bool) {
        self.park_for_skip();
        // Otherwise a still-loading track whose probe answers later would
        // start playing after the queue already ran out (status Stopped,
        // track None, audio playing).
        self.cancel_pending_start();
        self.resume_at = None;
        let next = if forward {
            self.queue.next()
        } else {
            self.queue.previous()
        };
        if next.is_some() {
            self.start_current(None);
        } else {
            self.shutdown_parked();
            self.stop_at_end_of_queue(forward);
        }
    }

    /// The queue ran out. Normally playback just stops; with autoplay on and
    /// a similar-tracks lookup still running, wait for it instead (the radio
    /// then starts on arrival — see `poll_autoplay`). Going *back* past the
    /// first track never autoplays.
    fn stop_at_end_of_queue(&mut self, forward: bool) {
        if forward && self.prefs.autoplay() && self.autoplay.inflight.is_some() {
            self.autoplay.start_on_arrival = true;
            self.status = PlaybackStatus::Loading;
        } else {
            self.status = PlaybackStatus::Stopped;
        }
    }

    fn seek(&mut self, seconds: f64) {
        let seconds = seconds.max(0.0);

        // Still loading: just move the target the decoder will seek to the
        // moment it's ready, nothing to flush yet.
        if let Some(p) = &mut self.pending_start {
            p.at = Some(seconds);
            return;
        }

        // Drain decoder events first: if the decoder already finished (its
        // `Eof` still unread), the in-place path below would hand a fresh
        // ring to a dead decoder and leave playback silent. Draining moves
        // that state into `pending`/`feeder`, so the restart branch runs.
        self.pump_decode_events();

        let Some(mut pb) = self.playback.take() else {
            if self.queue.current_id().is_some() {
                self.resume_at = Some(seconds);
            }
            return;
        };
        let paused = pb.render.paused.load(Ordering::Relaxed);

        // If the audible track's decoder already finished (we're in the
        // ring's tail with the next track queued behind it), the feeder
        // belongs to the next track: restart the audible track instead.
        if pb.pending.is_some() || pb.feeder.is_none() {
            pb.shutdown();
            self.start_current(Some(seconds));
            if let Some(p) = &mut self.pending_start {
                p.start_paused = paused;
            }
            return;
        }

        // Same track, so always the same negotiated output format (format
        // only ever changes at a track boundary — see `check_boundary`):
        // hand the render thread a fresh, already-seeked ring and let it
        // flush the device buffer + restart on the *same* open stream
        // (`RenderHandle::seek_tx`, serviced by `output::windows::render_loop`)
        // instead of reopening the device, which costs ~25-70ms and can
        // click. The render thread resets its played-frames counter to 0 on
        // the swap, matching `current.start_frame` being reset to 0 here.
        let (producer, consumer) =
            super::ring::new_pcm_ring(pb.negotiated.sample_rate, pb.negotiated.bytes_per_frame());
        // Decoder first: if it's gone (exited between the drain above and
        // now), nothing has been swapped yet and a full restart is clean.
        let decoder_alive = pb
            .feeder
            .as_ref()
            .is_some_and(|f| f.commands.send(DecodeCommand::Seek { seconds, producer }).is_ok());
        if !decoder_alive || pb.render.send_ring(consumer).is_err() {
            // Decoder or render thread died underneath us: full reopen.
            pb.shutdown();
            self.start_current(Some(seconds));
            if let Some(p) = &mut self.pending_start {
                p.start_paused = paused;
            }
            return;
        }
        pb.current.start_frame = 0;
        pb.current.start_offset = seconds;
        pb.seeks_sent += 1;
        self.playback = Some(pb);
        self.status = if paused {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        };
    }

    /// One press of the loop button (see `audio::abloop`). Needs something
    /// audible: with nothing playing there is no position to mark.
    fn loop_cycle(&mut self) {
        let Some(cur) = self.queue.current_id().map(str::to_string) else { return };
        if self.playback.is_none() {
            return;
        }
        if self.loop_track.as_deref() != Some(cur.as_str()) {
            self.ab_loop.clear();
        }
        self.loop_track = Some(cur);
        let pos = self.position_sec();
        self.ab_loop.cycle(pos);
    }

    /// Clears the loop when the current track changed; jumps back to A when
    /// playback reached B. Cheap (two `Option` reads) when no loop is set.
    fn loop_tick(&mut self) {
        if self.ab_loop.a.is_none() {
            return;
        }
        if self.queue.current_id() != self.loop_track.as_deref() {
            self.ab_loop.clear();
            self.loop_track = None;
            return;
        }
        if self.status != PlaybackStatus::Playing || self.playback.is_none() || self.pending_start.is_some() {
            return;
        }
        if let Some(a) = self.ab_loop.due(self.position_sec()) {
            self.seek(a);
        }
    }

    /// Reopens output (device/mode change) at the current position.
    fn restart_output(&mut self) {
        // A parked output was opened for the old device/mode.
        self.shutdown_parked();
        if self.playback.is_some() {
            let at = self.position_sec();
            let paused = self.status == PlaybackStatus::Paused;
            self.restart_current_reusing_downloads(at, paused);
        }
    }

    /// Tears playback down and starts the current track again at `at`,
    /// handing the running download to the new decoder (before the old one
    /// is dropped, so it never loses its last holder): same bytes, no second
    /// fetch of the track. The next track's prefetch is kept aside meanwhile
    /// and put back once the reader is consumed, so it isn't dropped and
    /// downloaded again either.
    fn restart_current_reusing_downloads(&mut self, at: f64, paused: bool) {
        let next_prefetch = self.prefetched.take();
        self.reuse_download_for_current();
        self.teardown_playback();
        if self.queue.current_id().is_some() {
            self.start_current(Some(at));
            if paused {
                self.pause();
            }
        }
        if self.prefetched.is_none() {
            self.prefetched = next_prefetch;
        }
    }

    /// Parks a fresh reader over the current track's existing download in
    /// the `prefetched` slot, which `take_or_open` serves first.
    fn reuse_download_for_current(&mut self) {
        let Some(id) = self.queue.current_id().map(str::to_string) else { return };
        if let Some(ms) = self.source.reattach(&id) {
            self.prefetched = Some((id, ms));
        }
    }

    fn queue_edited(&mut self) {
        if let Some(Pending { queue_after, .. }) =
            self.playback.as_mut().and_then(|pb| pb.pending.as_mut())
        {
            *queue_after = None;
        }
    }

    /// Spawns a decode thread for `id` and returns its command/event channels
    /// immediately — `TrackSource::open` only opens a local temp file or
    /// starts a background download, so this never blocks; the probe itself
    /// (which can block on a stalled HTTP connection) runs on the spawned
    /// thread and is polled, never awaited synchronously here.
    fn spawn_probe(&mut self, id: &str) -> Result<(Sender<DecodeCommand>, Receiver<DecodeEvent>), String> {
        let media_source = self
            .take_or_open(id)
            .map_err(|e| format!("open {id}: {e}"))?;
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (evt_tx, evt_rx) = std::sync::mpsc::channel();
        decode::spawn_decoder(media_source, None, cmd_rx, evt_tx);
        Ok((cmd_tx, evt_rx))
    }

    /// Blocking wait for a spawned probe's format — used only for the
    /// gapless hand-off's next-track probe at a track boundary
    /// (`handle_track_eof`), which needs the next format to decide
    /// gapless-continue vs. reopen *before* the boundary is reached. Unlike
    /// `start_current`'s probe (see `PendingStart`), this one can still
    /// briefly block `pump()` (bounded by `FORMAT_TIMEOUT`) if a track
    /// boundary coincides with a stalled network probe for the next track —
    /// a narrower window than blocking every `PlayQueue`/`Next`/`Previous`.
    fn open_decoder_blocking(&mut self, id: &str) -> Result<(Feeder, SourceFormat), String> {
        let (cmd_tx, evt_rx) = self.spawn_probe(id)?;
        match evt_rx.recv_timeout(FORMAT_TIMEOUT) {
            Ok(DecodeEvent::FormatReady(fmt)) => Ok((
                Feeder {
                    commands: cmd_tx,
                    events: evt_rx,
                },
                fmt,
            )),
            Ok(DecodeEvent::Error(e)) => Err(format!("{id}: {e}")),
            _ => {
                let _ = cmd_tx.send(DecodeCommand::Stop);
                Err(format!("{id}: decoder did not report a format in time"))
            }
        }
    }

    /// Negotiates and opens the output for `source` (or reuses `known`
    /// as-is), falling back to shared mode if an exclusive open fails.
    fn open_output(
        &mut self,
        source: SourceFormat,
        known: Option<NegotiatedFormat>,
        start_paused: bool,
        audible_id: &str,
    ) -> Option<OpenedOutput> {
        let mut negotiated = match known {
            Some(n) => n,
            None => {
                let neg =
                    output::negotiate(self.output_device_id.clone(), source, self.output_mode_pref);
                self.exclusive_error = neg.exclusive_error;
                neg.format
            }
        };
        loop {
            let (producer, consumer) =
                super::ring::new_pcm_ring(negotiated.sample_rate, negotiated.bytes_per_frame());
            let (mono_tx, mono_rx) =
                RingBuffer::<f32>::new((negotiated.sample_rate as usize / 4).max(256));
            let peaks = Arc::new(output::PeakMeter::default());
            let (render_tx, render_rx) = std::sync::mpsc::channel();
            match output::spawn_render(
                self.output_device_id.clone(),
                negotiated,
                consumer,
                Some(output::MonoTap {
                    mono: mono_tx,
                    peaks: peaks.clone(),
                    active: self.prefs.spectrum_active(),
                }),
                render_tx,
                start_paused,
                self.effective_gain_for(Some(audible_id)),
            ) {
                Ok(render) => {
                    let spectrum_stop = Arc::new(AtomicBool::new(false));
                    spawn_spectrum_thread(
                        mono_rx,
                        peaks,
                        negotiated.sample_rate,
                        self.events.clone(),
                        spectrum_stop.clone(),
                        self.prefs.spectrum_active(),
                    );
                    return Some(OpenedOutput {
                        negotiated,
                        producer,
                        render,
                        render_events: render_rx,
                        spectrum_stop,
                    });
                }
                Err(reason) if negotiated.mode == OutputMode::Exclusive => {
                    log::warn!("exclusive open failed ({reason}); falling back to shared mode");
                    self.exclusive_error = Some(format!(
                        "Exclusive mode could not start ({reason}), so this track plays in shared mode."
                    ));
                    negotiated = output::shared_format(source);
                }
                Err(reason) => {
                    log::error!("output open failed: {reason}");
                    if self.exclusive_error.is_none()
                        && self.output_mode_pref == OutputMode::Exclusive
                    {
                        self.exclusive_error = Some(reason);
                    }
                    return None;
                }
            }
        }
    }

    /// Starts loading whatever the queue says is current, optionally seeking
    /// to `at` seconds once ready. Never blocks: spawns the probe and
    /// records it as `pending_start`; `poll_pending_start` (from `pump()`)
    /// finishes the job once the format arrives (or fails/times out) — see
    /// `PendingStart`'s doc comment for why.
    fn start_current(&mut self, at: Option<f64>) {
        self.cancel_pending_start();
        let Some(id) = self.queue.current_id().map(|s| s.to_string()) else {
            self.status = PlaybackStatus::Stopped;
            return;
        };
        // Lazy metadata resolution: `PlayAll` deliberately doesn't eagerly
        // resolve a (potentially 50k-id) queue up front, so whatever becomes
        // current here is what actually gets looked up — a no-op (cache
        // check only) for the common `PlayQueue` path, which already
        // resolved everything.
        self.resolve_tracks(std::slice::from_ref(&id));
        self.bound_track_cache();
        self.status = PlaybackStatus::Loading;
        self.resume_at = None;

        match self.spawn_probe(&id) {
            Ok((commands, events)) => {
                self.pending_start = Some(PendingStart {
                    id,
                    commands,
                    events,
                    started: Instant::now(),
                    at,
                    start_paused: false,
                });
            }
            Err(e) => self.fail_current_track(&e),
        }
    }

    fn cancel_pending_start(&mut self) {
        if let Some(p) = self.pending_start.take() {
            p.stop();
        }
    }

    /// Polls the in-flight probe (if any) without blocking: moves to
    /// `begin_playback` once the format is ready, otherwise fails (and skips
    /// to the next queued track) on a decode error, a dead decoder thread, or
    /// `FORMAT_TIMEOUT` with no response at all (a hung/stalled connection).
    fn poll_pending_start(&mut self) {
        let timed_out = self
            .pending_start
            .as_ref()
            .is_some_and(|p| p.started.elapsed() > FORMAT_TIMEOUT);
        if timed_out {
            let p = self.pending_start.take().expect("checked above");
            p.stop();
            self.fail_current_track(&format!("{}: timed out waiting for the stream", p.id));
            return;
        }
        let polled = match self.pending_start.as_ref() {
            Some(p) => p.events.try_recv(),
            None => return,
        };
        match polled {
            Ok(DecodeEvent::FormatReady(fmt)) => {
                let p = self.pending_start.take().expect("checked above");
                self.begin_playback(p, fmt);
            }
            Ok(DecodeEvent::Error(e)) => {
                let p = self.pending_start.take().expect("checked above");
                self.fail_current_track(&format!("{}: {e}", p.id));
            }
            Ok(DecodeEvent::Eof { .. }) => {} // not sent before `Begin`; ignore
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                let p = self.pending_start.take().expect("checked above");
                self.fail_current_track(&format!("{}: decoder thread exited unexpectedly", p.id));
            }
        }
    }

    /// Opens the output device for a probe that just reported its format and
    /// starts decoding into it — the second half of what `start_current`
    /// used to do synchronously. Then prefetches the next track.
    fn begin_playback(&mut self, p: PendingStart, source_format: SourceFormat) {
        let feeder = Feeder {
            commands: p.commands,
            events: p.events,
        };
        let playback = match self.parked.take() {
            Some(pb) if self.can_switch_in_place(&pb, &source_format) => {
                match self.switch_in_place(pb, feeder, &p.id, source_format, p.at, p.start_paused) {
                    Ok(pb) => Some(pb),
                    // The parked render thread was gone: it is shut down, the
                    // feeder comes back for a normal open.
                    Err(feeder) => self.open_playback(feeder, &p.id, source_format, p.at, p.start_paused),
                }
            }
            other => {
                if let Some(pb) = other {
                    pb.shutdown();
                }
                self.open_playback(feeder, &p.id, source_format, p.at, p.start_paused)
            }
        };
        let Some(pb) = playback else {
            self.fail_current_track(&format!("{}: output device open failed", p.id));
            return;
        };
        self.playback = Some(pb);
        self.status = if p.start_paused {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        };
        self.last_error = None; // a track is actually playing again
        self.consecutive_failures = 0;
        let queue = self.queue.clone();
        self.prefetch_after(&queue);
    }

    /// The parked output can carry `fmt` as it is: same user-requested mode
    /// (a shared fallback never silently stands in for a requested exclusive
    /// stream) and the gapless rule (`can_continue`) for rate/channels/container.
    fn can_switch_in_place(&self, pb: &Playback, fmt: &SourceFormat) -> bool {
        if pb.negotiated.mode != self.output_mode_pref || !can_continue(&pb.negotiated, fmt) {
            return false;
        }
        // Following the system default: the default may have moved since the
        // output was opened (a skip used to reopen on the new default).
        // An unknown default (lookup failed) keeps the output.
        match (&pb.opened_default, self.current_default_id()) {
            (Some(opened), Some(now)) => *opened == now,
            _ => true,
        }
    }

    /// Feeds the new track into the parked output through a fresh ring (the
    /// in-place seek channel), no device reopen. If the render thread is
    /// gone the parked output is shut down and the feeder handed back.
    fn switch_in_place(
        &mut self,
        mut pb: Playback,
        feeder: Feeder,
        id: &str,
        fmt: SourceFormat,
        at: Option<f64>,
        start_paused: bool,
    ) -> Result<Playback, Feeder> {
        let (producer, consumer) =
            super::ring::new_pcm_ring(pb.negotiated.sample_rate, pb.negotiated.bytes_per_frame());
        // Gain and pause state first, so the new ring's first frame is
        // already rendered at the new track's ReplayGain / in the right state.
        pb.render.set_volume(self.effective_gain_for(Some(id)));
        pb.render.set_paused(start_paused);
        if pb.render.send_ring(consumer).is_err() {
            pb.shutdown();
            return Err(feeder);
        }
        let start_offset = Self::begin_decoder(&feeder, pb.negotiated.container, producer, at);
        pb.seeks_sent += 1;
        pb.current = Slot {
            id: id.to_string(),
            format: fmt,
            start_frame: 0,
            start_offset,
        };
        pb.feeder = Some(feeder);
        pb.pending = None;
        Ok(pb)
    }

    /// Starts `feeder` into `producer`, seeking to `at` first when asked.
    /// Returns the start offset in seconds.
    fn begin_decoder(feeder: &Feeder, container: Container, producer: Producer<u8>, at: Option<f64>) -> f64 {
        match at {
            Some(sec) if sec > 0.0 => {
                // Begin with a zero-capacity producer, then seek into the
                // real ring: the decoder handles both before its first packet.
                let (dummy, _) = RingBuffer::new(0);
                let _ = feeder.commands.send(DecodeCommand::Begin { container, producer: dummy });
                let _ = feeder.commands.send(DecodeCommand::Seek { seconds: sec, producer });
                sec
            }
            _ => {
                let _ = feeder.commands.send(DecodeCommand::Begin { container, producer });
                0.0
            }
        }
    }

    /// Negotiates and opens a fresh output for the probed track and starts
    /// decoding into it. `None` (feeder stopped) if the device won't open.
    fn open_playback(
        &mut self,
        feeder: Feeder,
        id: &str,
        fmt: SourceFormat,
        at: Option<f64>,
        start_paused: bool,
    ) -> Option<Playback> {
        let Some(out) = self.open_output(fmt, None, start_paused, id) else {
            feeder.stop();
            return None;
        };
        let start_offset = Self::begin_decoder(&feeder, out.negotiated.container, out.producer, at);
        Some(Playback {
            negotiated: out.negotiated,
            render: out.render,
            render_events: out.render_events,
            spectrum_stop: out.spectrum_stop,
            device_name: self.device_name(),
            opened_default: self.current_default_id(),
            current: Slot {
                id: id.to_string(),
                format: fmt,
                start_frame: 0,
                start_offset,
            },
            feeder: Some(feeder),
            pending: None,
            seeks_sent: 0,
        })
    }

    /// Records `reason` as `PlayerState.error` and skips to the next queued
    /// track (per the audio reviewer's requirement: a stream failure/stall
    /// surfaces as an error *and* playback moves on rather than hanging).
    /// Iterative, not recursive: a synchronous open failure inside the
    /// `advance` below re-enters here, which only sets `skip_pending` for the
    /// loop. Stops (keeping the queue position, so Play retries) after
    /// `MAX_CONSECUTIVE_FAILURES` in a row - repeat one/all or an offline
    /// session would otherwise skip forever.
    fn fail_current_track(&mut self, reason: &str) {
        log::error!("playback error: {reason}");
        self.listen_sample();
        self.listen.note(EndReason::Error);
        self.last_error = Some(reason.to_string());
        self.status = PlaybackStatus::Stopped;
        self.consecutive_failures += 1;
        if self.in_failure_chain {
            self.skip_pending = true;
            return;
        }
        self.in_failure_chain = true;
        loop {
            if self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                self.teardown_playback();
                self.cancel_pending_start();
                self.status = PlaybackStatus::Stopped;
                self.last_error = Some(format!("{reason} (stopped after {} failed tracks in a row)", self.consecutive_failures));
                break;
            }
            self.skip_pending = false;
            self.advance(true);
            if !self.skip_pending {
                break;
            }
        }
        self.in_failure_chain = false;
    }

    /// The system default endpoint, only while following it (no device chosen).
    fn current_default_id(&self) -> Option<String> {
        if self.output_device_id.is_some() {
            return None;
        }
        (self.default_id_lookup)()
    }

    fn device_name(&self) -> Option<String> {
        // Keyed by the endpoint that actually opens: the chosen id, or while
        // following the default, the current default's id (a cheap lookup;
        // the Windows default can change behind our back). If that lookup
        // fails, resolve without caching.
        let key = match &self.output_device_id {
            Some(id) => Some(id.clone()),
            None => (self.default_id_lookup)(),
        };
        if let (Some(key), Some((id, name))) = (&key, &*self.device_name_cache.borrow())
            && key == id {
                return name.clone();
            }
        let name = self.resolve_device_name(key.as_deref());
        if let Some(key) = key {
            *self.device_name_cache.borrow_mut() = Some((key, name.clone()));
        }
        name
    }

    fn resolve_device_name(&self, endpoint: Option<&str>) -> Option<String> {
        let devices = output::list_devices();
        endpoint
            .and_then(|id| devices.iter().find(|d| d.id == id))
            .or_else(|| devices.iter().find(|d| d.is_default))
            .map(|d| d.name.clone())
    }

    /// Returns a prefetched `MediaSource` for `id` if one's already
    /// downloading, otherwise opens it fresh.
    fn take_or_open(
        &mut self,
        id: &str,
    ) -> std::io::Result<Box<dyn symphonia::core::io::MediaSource>> {
        if let Some((pid, _)) = &self.prefetched
            && pid == id {
                return Ok(self.prefetched.take().expect("checked above").1);
            }
        self.prefetched = None; // drops (and deletes) a stale prefetch
        self.source.open(id)
    }

    /// Starts downloading the track after `queue`'s current one to disk.
    fn prefetch_after(&mut self, queue: &Queue) {
        let mut peek = queue.clone();
        let Some(next_id) = peek.next().map(|s| s.to_string()) else {
            return;
        };
        if queue.current_id() == Some(next_id.as_str()) {
            return; // repeat-one: the source is re-opened from cache/server on restart
        }
        if self
            .prefetched
            .as_ref()
            .is_some_and(|(id, _)| *id == next_id)
        {
            return;
        }
        self.prefetched = None;
        match self.source.open(&next_id) {
            Ok(ms) => self.prefetched = Some((next_id, ms)),
            Err(e) => log::warn!("prefetch of {next_id} failed: {e}"),
        }
    }

    fn teardown_playback(&mut self) {
        self.shutdown_parked();
        if let Some(pb) = self.playback.take() {
            pb.shutdown();
        }
    }

    fn shutdown_parked(&mut self) {
        if let Some(pb) = self.parked.take() {
            pb.shutdown();
        }
    }

    /// User skip (Next/Previous/JumpTo/remove-current): silence the audible
    /// track *without* closing the device. The old decoder is stopped, the
    /// render thread gets an empty ring through the in-place seek channel
    /// (flushing the device buffer), and the `Playback` is parked until the
    /// new track's format is known: `begin_playback` then continues on the
    /// open stream when the format fits, else tears it down and reopens
    /// exactly as before. To everything else the engine looks like it has
    /// no playback (same as after `teardown_playback`).
    fn park_for_skip(&mut self) {
        let Some(mut pb) = self.playback.take() else {
            return; // already loading: an existing parked output stays parked
        };
        self.shutdown_parked();
        let lost = matches!(pb.render_events.try_recv(), Ok(RenderEvent::DeviceLost(_)));
        if let Some(f) = pb.feeder.take() {
            f.stop();
        }
        if let Some(Pending { reopen: Some(f), .. }) = pb.pending.take() {
            f.stop();
        }
        // The empty ring's producer is dropped right away: the render thread
        // just writes silence for it.
        let (producer, consumer) =
            super::ring::new_pcm_ring(pb.negotiated.sample_rate, pb.negotiated.bytes_per_frame());
        drop(producer);
        if lost || pb.render.send_ring(consumer).is_err() {
            pb.shutdown();
            return;
        }
        pb.seeks_sent += 1;
        self.parked = Some(pb);
    }

    // --- Event pump --------------------------------------------------------

    fn pump(&mut self) {
        self.poll_pending_start();
        self.check_parked();
        self.pump_render_events();
        self.check_boundary();
        self.pump_decode_events();
    }

    /// A parked output only lives while its replacement track is loading,
    /// and only while its device is still there.
    fn check_parked(&mut self) {
        let Some(pb) = &self.parked else { return };
        let lost = matches!(pb.render_events.try_recv(), Ok(RenderEvent::DeviceLost(_)));
        if lost || self.pending_start.is_none() {
            self.shutdown_parked();
        }
    }

    fn pump_render_events(&mut self) {
        let Some(pb) = &self.playback else { return };
        let Ok(RenderEvent::DeviceLost(reason)) = pb.render_events.try_recv() else {
            return;
        };
        log::warn!("output device lost: {reason}");
        self.device_name_cache.take();
        let at = pb.position_sec();
        self.teardown_playback();
        // Keep the track and position so Play resumes where we were.
        self.resume_at = Some(at);
        self.status = PlaybackStatus::Paused;
        self.exclusive_error = Some(format!("output device lost: {reason}"));
    }

    /// Drains the feeding decoder's events. Held back while a boundary is
    /// pending: a very short next track's own EOF must wait until the
    /// render side has reached its start.
    fn pump_decode_events(&mut self) {
        loop {
            let Some(pb) = &self.playback else { return };
            if pb.pending.is_some() {
                return;
            }
            let Some(feeder) = &pb.feeder else { return };
            match feeder.events.try_recv() {
                Ok(DecodeEvent::Eof {
                    producer,
                    frames_pushed,
                }) => self.handle_track_eof(producer, frames_pushed),
                Ok(DecodeEvent::Error(e)) => log::error!("decode error for {}: {e}", pb.current.id),
                Ok(DecodeEvent::FormatReady(_)) => {}
                Err(_) => return,
            }
        }
    }

    /// The feeding decoder finished. Works out where the boundary is in the
    /// ring and lines up the next track behind it — the switch itself
    /// happens in `check_boundary` once the boundary has been played.
    fn handle_track_eof(&mut self, producer: Producer<u8>, frames_pushed: u64) {
        let Some(mut pb) = self.playback.take() else {
            return;
        };
        pb.feeder = None; // that decoder thread has exited
        let boundary = pb.current.start_frame + frames_pushed;

        let mut queue_after = self.queue.clone();
        let next_id = queue_after.next().map(|s| s.to_string());
        // The gapless path never goes through `start_current`, and a
        // rolling-window source's next id may have just been drawn by the
        // refill above: resolve it now so the track change (PlayerState,
        // media session) carries real metadata, not the stub.
        if let Some(id) = &next_id {
            self.resolve_tracks(std::slice::from_ref(id));
        }
        let mut pending = Pending {
            boundary,
            next: None,
            reopen: None,
            queue_after: Some(queue_after),
        };

        if let Some(next_id) = next_id {
            match self.open_decoder_blocking(&next_id) {
                Ok((feeder, fmt)) => {
                    if can_continue(&pb.negotiated, &fmt) {
                        // True gapless: the same ring producer continues
                        // with the next track; render never notices.
                        let _ = feeder.commands.send(DecodeCommand::Begin {
                            container: pb.negotiated.container,
                            producer,
                        });
                        pb.feeder = Some(feeder);
                        pending.next = Some(Slot {
                            id: next_id,
                            format: fmt,
                            start_frame: boundary,
                            start_offset: 0.0,
                        });
                    } else {
                        log::info!(
                            "format change {} Hz {} -> {} Hz {:?}: device reopens after the current track",
                            pb.negotiated.sample_rate,
                            pb.negotiated.container.label(),
                            fmt.sample_rate,
                            fmt.bit_depth
                        );
                        drop(producer);
                        pending.next = Some(Slot {
                            id: next_id,
                            format: fmt,
                            start_frame: 0,
                            start_offset: 0.0,
                        });
                        pending.reopen = Some(feeder);
                    }
                }
                Err(e) => {
                    log::error!("could not open next track: {e}");
                    drop(producer);
                }
            }
        } else {
            drop(producer);
        }
        if let Some(q) = pending.queue_after.clone() {
            self.prefetch_after(&q);
        }
        pb.pending = Some(pending);
        self.playback = Some(pb);
    }

    /// Commits a pending track change once the render side has played up to
    /// the boundary: the queue/UI advance exactly when the listener hears
    /// the new track.
    fn check_boundary(&mut self) {
        let Some(pb) = &self.playback else { return };
        let Some(pending) = &pb.pending else { return };
        // A ring swap (skip/seek) the render thread has not applied yet: its
        // frame counter still belongs to the old ring.
        if pb.render.seeks_applied.load(Ordering::Acquire) < pb.seeks_sent {
            return;
        }
        if pb.played_frames() < pending.boundary {
            return;
        }
        let mut pb = self.playback.take().expect("checked above");
        let pending = pb.pending.take().expect("checked above");

        // Advance the real queue.
        let queue_was_clean = pending.queue_after.is_some();
        match pending.queue_after {
            Some(q) => self.queue = q,
            None => {
                self.queue.next();
            }
        }
        self.bound_track_cache();
        let Some(next) = pending.next else {
            pb.shutdown();
            if self.queue.current_id().is_some() {
                // The queue gained a track during the ring's tail (autoplay's
                // lookup answered too late for a gapless hand-off, or the user
                // enqueued something): play it, reopening the output.
                self.start_current(None);
            } else {
                self.stop_at_end_of_queue(true);
            }
            return;
        };
        if !queue_was_clean && self.queue.current_id() != Some(next.id.as_str()) {
            // Queue edited during the tail: play what the queue now says.
            if let Some(f) = pending.reopen {
                f.stop();
            }
            pb.shutdown();
            self.start_current(None);
            return;
        }

        match pending.reopen {
            None => {
                log::info!(
                    "gapless track change to {} at ring frame {} (underrun gap frames in this stream: {})",
                    next.id,
                    pending.boundary,
                    pb.render.gap_frames.load(Ordering::Relaxed)
                );
                pb.current = next;
                self.playback = Some(pb);
                // ReplayGain follows the audible track: switched here, on the
                // engine's 10 ms boundary poll (see `apply_gain`), i.e. within
                // ~10-20 ms of the new track becoming audible.
                self.apply_gain();
            }
            Some(feeder) => {
                let reopen_started = Instant::now();
                let paused = pb.render.paused.load(Ordering::Relaxed);
                pb.spectrum_stop.store(true, Ordering::Relaxed);
                pb.render.stop_and_join();
                let Some(out) = self.open_output(next.format, None, paused, &next.id) else {
                    feeder.stop();
                    self.status = PlaybackStatus::Stopped;
                    return;
                };
                let _ = feeder.commands.send(DecodeCommand::Begin {
                    container: out.negotiated.container,
                    producer: out.producer,
                });
                log::info!(
                    "format change to {}: output reopened as {} in {:?}",
                    next.id,
                    out.negotiated.format_label(""),
                    reopen_started.elapsed()
                );
                self.playback = Some(Playback {
                    negotiated: out.negotiated,
                    render: out.render,
                    render_events: out.render_events,
                    spectrum_stop: out.spectrum_stop,
                    device_name: pb.device_name,
                    opened_default: self.current_default_id(),
                    current: next,
                    feeder: Some(feeder),
                    pending: None,
                    seeks_sent: 0,
                });
            }
        }
    }

    // --- State -------------------------------------------------------------

    /// Engine-authoritative: exclusive mode (native rate, bit-identical
    /// container) and a combined render gain (volume taper x ReplayGain) of
    /// exactly unity. See `PlayerState.bit_perfect`/`OutputStatus.bit_perfect`.
    fn is_bit_perfect(&self) -> bool {
        self.playback
            .as_ref()
            .is_some_and(|pb| pb.negotiated.is_bit_perfect_capable())
            && gain_is_unity(self.effective_gain_for(self.audible_id()))
    }

    // --- ReplayGain ----------------------------------------------------

    /// `(linear, dB)` ReplayGain for `id` under the current mode, `None` =
    /// unity (mode off, or the track has no usable tag in the local library).
    fn replay_gain_for(&self, id: &str) -> Option<(f32, f32)> {
        let mode = self.prefs.replay_gain();
        if mode == ReplayGainMode::Off {
            return None;
        }
        replaygain::replay_gain_factor(mode, self.track_cache.get(id).and_then(|t| t.replay_gain.as_ref()))
    }

    /// The one gain the render thread applies: the user's volume taper times
    /// the audible track's ReplayGain factor. Exactly 1.0 (no sample touched,
    /// bit-perfect) only when both are unity.
    fn effective_gain_for(&self, audible_id: Option<&str>) -> f32 {
        let rg = audible_id.and_then(|id| self.replay_gain_for(id)).map_or(1.0, |(lin, _)| lin);
        output::volume_to_gain(self.volume) * rg
    }

    fn audible_id(&self) -> Option<&str> {
        self.playback.as_ref().map(|pb| pb.current.id.as_str())
    }

    /// Pushes the combined gain for the *audible* track to the render thread.
    /// Called on volume changes, ReplayGain mode changes and when a track
    /// becomes audible. Gapless boundaries are committed by `check_boundary`
    /// (polled every `BOUNDARY_TICK` = 10 ms while a boundary is pending), so
    /// the new track's gain takes effect within ~10 ms + one render period of
    /// its first sample — the granularity is documented, not frame-exact.
    fn apply_gain(&self) {
        if let Some(pb) = &self.playback {
            pb.render.set_volume(self.effective_gain_for(Some(&pb.current.id)));
        }
    }

    /// The ReplayGain dB currently applied to the audible track (after the
    /// peak clamp), only when it actually changes the signal.
    fn applied_replay_gain_db(&self) -> Option<f32> {
        let (_, db) = self.audible_id().and_then(|id| self.replay_gain_for(id))?;
        (db.abs() >= 0.05).then_some(db)
    }

    /// Just the codec (e.g. `FLAC`): `PlayerState`/`OutputStatus` carry the
    /// mode, bit depth and rate as separate fields and the UI composes the
    /// readout from them, so repeating them here doubled up as
    /// "SHARED · 32/44 · SHARED · 32/44100 · FLAC".
    fn format_label(&self) -> Option<String> {
        self.playback.as_ref().map(|pb| pb.current.format.codec_label())
    }

    fn output_status(&self) -> OutputStatus {
        match &self.playback {
            Some(pb) => OutputStatus {
                device_id: self.output_device_id.clone(),
                device_name: pb.device_name.clone(),
                mode: pb.negotiated.mode,
                sample_rate: Some(pb.negotiated.sample_rate),
                bit_depth: Some(pb.negotiated.container.valid_bits()),
                format_label: self.format_label(),
                bit_perfect: self.is_bit_perfect(),
                exclusive_error: self.exclusive_error.clone(),
            },
            None => OutputStatus {
                device_id: self.output_device_id.clone(),
                device_name: None,
                mode: self.output_mode_pref,
                sample_rate: None,
                bit_depth: None,
                format_label: None,
                bit_perfect: false,
                exclusive_error: self.exclusive_error.clone(),
            },
        }
    }

    /// Resolves any of `ids` not already in `track_cache` from the local
    /// library DB (one query, batched), merging the results in. Called only
    /// from queue-mutating commands (`PlayQueue`/`Enqueue`) — never from the
    /// player-state/render-tick hot path — so this is at most one SQLite
    /// read per user action. An id with no local row (not yet synced, or
    /// from a different account) simply has no cache entry; `track_for`
    /// falls back to an honestly-empty stub for it rather than inventing
    /// metadata.
    /// Drops cached metadata for ids no longer in the queue once the cache
    /// has outgrown it by more than a page or so. A rolling-window source
    /// keeps pruning played history (`Queue::prune_history`), so without
    /// this an hours-long shuffle would cache every track it ever played.
    /// Cheap (a length check) except when it actually trims, which is at
    /// most once every `TRACK_CACHE_SLACK` new ids.
    fn bound_track_cache(&mut self) {
        const TRACK_CACHE_SLACK: usize = 128;
        if self.track_cache.len() <= self.queue.len() + TRACK_CACHE_SLACK {
            return;
        }
        let keep: std::collections::HashSet<&str> = self.queue.ids().collect();
        self.track_cache.retain(|id, _| keep.contains(id.as_str()));
    }

    fn resolve_tracks(&mut self, ids: &[String]) {
        let missing: Vec<String> = ids
            .iter()
            .filter(|id| !self.track_cache.contains_key(id.as_str()))
            .cloned()
            .collect();
        if missing.is_empty() {
            return;
        }
        match library::get_tracks_by_ids(&self.db, &missing) {
            Ok(tracks) => {
                for t in tracks {
                    self.track_cache.insert(t.id.clone(), t);
                }
            }
            Err(e) => log::warn!("track metadata lookup failed: {}", e.message),
        }
    }

    /// The full `Track` for `id`: real metadata from `track_cache` (the
    /// local library DB) when available, overlaid with whatever the decoder
    /// has actually found for the currently *audible* track (format/
    /// duration) — more precise than the server's metadata (exact bit depth,
    /// trimmed duration) and always real, never fabricated. An id with
    /// neither a cache entry nor live decode info yet (very first tick after
    /// `PlayQueue`, before `resolve_tracks`'s query lands — effectively never
    /// observable since that query runs synchronously in the same command)
    /// falls back to an honestly-empty stub rather than inventing data.
    fn track_for(&self, id: &str) -> Track {
        let mut track = self.track_cache.get(id).cloned().unwrap_or_else(|| Track {
            id: id.to_string(),
            title: id.to_string(),
            artist: String::new(),
            artist_id: None,
            album: None,
            album_id: None,
            album_artist: None,
            track_number: None,
            disc_number: None,
            year: None,
            genre: None,
            duration: 0.0,
            format: TrackFormat {
                suffix: String::new(),
                bit_rate: None,
                bit_depth: None,
                sampling_rate: 0,
                channel_count: 2,
            },
            cover_art: None,
            starred: None,
            rating: None,
            size: None,
            replay_gain: None,
            path: None,
            date_added: None,
        });
        if let Some(fmt) = self
            .playback
            .as_ref()
            .filter(|pb| pb.current.id == id)
            .map(|pb| pb.current.format)
        {
            track.format.suffix = fmt.codec_label().to_lowercase();
            track.format.bit_depth = fmt.bit_depth;
            track.format.sampling_rate = fmt.sample_rate;
            track.format.channel_count = fmt.channels;
            let decoded = fmt.duration_sec();
            if decoded > 0.0 {
                track.duration = decoded;
            }
        }
        track
    }

    fn player_state(&self) -> PlayerState {
        // While a boundary is pending the queue still points at the audible
        // track, so `current_id` is always what the listener hears.
        let track = self.queue.current_id().map(|id| self.track_for(id));
        let duration = self
            .playback
            .as_ref()
            .map(|pb| pb.current.format.duration_sec())
            .filter(|d| *d > 0.0)
            .or_else(|| track.as_ref().map(|t| t.duration))
            .unwrap_or(0.0);
        let mut position = self.position_sec();
        if duration > 0.0 {
            position = position.min(duration);
        }
        PlayerState {
            status: self.status,
            track,
            position_sec: position,
            duration_sec: duration,
            output_mode: self
                .playback
                .as_ref()
                .map(|pb| pb.negotiated.mode)
                .unwrap_or(self.output_mode_pref),
            sample_rate: self.playback.as_ref().map(|pb| pb.negotiated.sample_rate),
            bit_depth: self
                .playback
                .as_ref()
                .map(|pb| pb.negotiated.container.valid_bits()),
            float_mix: self
                .playback
                .as_ref()
                .is_some_and(|pb| is_float_mix(&pb.negotiated)),
            format_label: self.format_label(),
            bit_perfect: self.is_bit_perfect(),
            volume: self.volume,
            volume_db: output::volume_to_db(self.volume),
            shuffle: self.queue.shuffle,
            repeat: self.queue.repeat,
            queue_length: self.queue.len() as u32,
            error: self.last_error.clone(),
            replay_gain_db: self.applied_replay_gain_db(),
            // Only while the loop still belongs to the current entry (a state
            // built right after Next/Stop, before the next tick clears it).
            loop_a: self.ab_loop.a.filter(|_| self.loop_track.as_deref() == self.queue.current_id()),
            loop_b: self.ab_loop.b.filter(|_| self.loop_track.as_deref() == self.queue.current_id()),
        }
    }
}

/// Same threshold the render thread uses to skip the gain stage entirely, so
/// "unity" here means "not a single sample is touched".
fn gain_is_unity(g: f32) -> bool {
    (g - 1.0).abs() <= f32::EPSILON
}

// --- continuity: session persistence, restore, autoplay ---------------------

impl Engine {
    /// Called once per run-loop iteration, after `pump()`. Everything here is
    /// O(1) unless something actually changed.
    fn continuity_tick(&mut self) {
        self.loop_tick();
        self.observe_audible();
        self.poll_autoplay();
        self.maybe_request_autoplay();
        self.persist_tick();
    }

    /// Remembers what was actually audible (not merely queued/restored) for
    /// autoplay's "don't repeat the last ~50 tracks" rule.
    fn observe_audible(&mut self) {
        let aud = self.audible_id().map(str::to_string);
        if aud == self.last_audible {
            return;
        }
        if let Some(id) = &aud {
            self.recent_played.retain(|r| r != id);
            self.recent_played.push_back(id.clone());
            while self.recent_played.len() > RECENT_PLAYED_LIMIT {
                self.recent_played.pop_front();
            }
        }
        self.last_audible = aud;
    }

    // --- Autoplay --------------------------------------------------------

    fn reset_autoplay(&mut self) {
        self.autoplay = AutoplayState::default();
    }

    /// Starts the similar-tracks lookup the moment the *last* queued track is
    /// playing, so the answer is normally in before it ends (gapless).
    fn maybe_request_autoplay(&mut self) {
        let cur = self.queue.current_id().map(str::to_string);
        let check = autoplay::WantCheck {
            enabled: self.prefs.autoplay(),
            have_provider: self.autoplay_source.is_some(),
            playing_something: self.playback.is_some(),
            at_end: self.queue.is_at_end(),
            request_inflight: self.autoplay.inflight.is_some(),
            already_requested_for_current: cur.is_some() && self.autoplay.requested_for == cur,
        };
        if !autoplay::autoplay_wanted(&check) {
            return;
        }
        let (Some(seed), Some(provider)) = (cur, self.autoplay_source.clone()) else { return };
        self.autoplay.requested_for = Some(seed.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let lookup_id = seed.clone();
        let spawned = std::thread::Builder::new().name("sonix-autoplay".into()).spawn(move || {
            let _ = tx.send(provider.similar(&lookup_id));
        });
        match spawned {
            Ok(_) => self.autoplay.inflight = Some(InflightAutoplay { seed, rx }),
            Err(e) => log::warn!("autoplay lookup thread not started: {e}"),
        }
    }

    fn poll_autoplay(&mut self) {
        let Some(inflight) = &self.autoplay.inflight else { return };
        let items = match inflight.rx.try_recv() {
            Ok(items) => items,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Vec::new(),
        };
        let seed = inflight.seed.clone();
        self.autoplay.inflight = None;
        self.apply_autoplay(&seed, items);
    }

    /// Appends the lookup's answer behind the queue's last track as a `radio`
    /// source ("Autoplay · <seed title>"). Recently played / already queued ids
    /// are dropped; an empty result leaves the queue to end normally (and is
    /// not retried for the same track, so a dead-end radio can't loop).
    fn apply_autoplay(&mut self, seed: &str, items: Vec<(String, Option<String>)>) {
        // Only still "waiting" if nothing was started since the queue ran out
        // (a Previous/JumpTo/new queue in the meantime owns the status now).
        let waiting = std::mem::take(&mut self.autoplay.start_on_arrival)
            && self.queue.current_id().is_none()
            && self.playback.is_none()
            && self.pending_start.is_none();
        let queued: std::collections::HashSet<String> = self.queue.ids().map(str::to_string).collect();
        let (ids, artists) = autoplay::filter_candidates(items, &self.recent_played, &|id| queued.contains(id));
        if !self.prefs.autoplay() || ids.is_empty() {
            log::info!("autoplay: nothing to continue with after {seed}");
            if waiting {
                self.status = PlaybackStatus::Stopped;
            }
            return;
        }
        let title = self.track_cache.get(seed).map(|t| t.title.clone()).unwrap_or_else(|| seed.to_string());
        let label = format!("{}{title}", autoplay::LABEL_PREFIX);
        if self.queue.current_id() == Some(seed) {
            // Stale unless the queue is *still* about to end: the user may
            // have enqueued something or turned repeat on while the lookup
            // ran (the new last track then gets its own lookup).
            if !self.queue.is_at_end() {
                return;
            }
            // Still on the last track: line the radio up behind it so the
            // gapless hand-off picks it up. A boundary snapshot taken before
            // this answer is stale now.
            self.queue.continue_with_source(ipc::SourceKind::Radio, label, ids, artists);
            self.queue_edited();
            self.tracking.queue_dirty = true;
            // Start downloading the first radio track now, like
            // `begin_playback` does for an ordinary next track, so the EOF
            // probe in `handle_track_eof` doesn't wait on a cold stream.
            let queue = self.queue.clone();
            self.prefetch_after(&queue);
            log::info!("autoplay: queued similar tracks after {seed}");
        } else if waiting && self.queue.reopen_last(seed) {
            // The last track already ended (or Next was pressed on it) before
            // the answer arrived: start the radio now, with a gap.
            self.queue.continue_with_source(ipc::SourceKind::Radio, label, ids, artists);
            self.tracking.queue_dirty = true;
            if self.queue.next().is_some() {
                self.consecutive_failures = 0;
                self.start_current(None);
            } else {
                self.status = PlaybackStatus::Stopped;
            }
        } else if waiting {
            self.status = PlaybackStatus::Stopped;
        }
    }

    // --- Session persistence --------------------------------------------

    fn session_request(&mut self, force_full: bool, push: PushKind) -> SaveRequest {
        let stamp = self.queue.source_stamp();
        let queue = (force_full || self.tracking.queue_dirty || self.tracking.need_core).then(|| self.queue.snapshot_core());
        let pools = if queue.is_some() && (self.tracking.need_core || stamp != self.tracking.last_stamp) {
            self.queue.snapshot_pools()
        } else {
            None
        };
        if queue.is_some() {
            // Only a request that carries the core may advance the stamp: a
            // position-only save never writes pools, so recording its stamp
            // could let the next core go out without the pools it names
            // (restore would then drop the source as mismatched).
            self.tracking.last_stamp = stamp;
            self.tracking.need_core = false;
            self.tracking.queue_dirty = false;
            self.tracking.last_current = self.queue.current_id().map(str::to_string);
        }
        self.tracking.last_status = self.status;
        self.tracking.last_write = Instant::now();
        if push != PushKind::Final {
            self.tracking.activity = true;
        }
        SaveRequest {
            queue,
            pools,
            position_ms: (self.position_sec().max(0.0) * 1000.0) as u64,
            volume: self.volume,
            push,
        }
    }

    /// Debounced writes: queue/track/pause changes (>= 1.5 s apart), plus a
    /// position refresh every 10 s while playing. Only hands a request to the
    /// writer thread — no I/O here.
    fn persist_tick(&mut self) {
        let Some(session) = self.session.clone() else { return };
        if !self.tracking.enabled {
            if self.queue.is_empty() {
                return;
            }
            self.tracking.enabled = true;
            self.tracking.queue_dirty = true;
        }
        let cur = self.queue.current_id().map(str::to_string);
        if cur != self.tracking.last_current {
            self.tracking.queue_dirty = true;
        }
        let since = self.tracking.last_write.elapsed();
        let paused_now = matches!(self.status, PlaybackStatus::Paused | PlaybackStatus::Stopped)
            && self.status != self.tracking.last_status;
        let kind = if (self.tracking.queue_dirty && since >= SESSION_WRITE_DEBOUNCE) || paused_now {
            Some(PushKind::Event)
        } else if self.status == PlaybackStatus::Playing && since >= SESSION_POSITION_INTERVAL {
            Some(PushKind::Periodic)
        } else {
            None
        };
        if let Some(kind) = kind {
            let req = self.session_request(false, kind);
            session.save(req);
        }
    }

    /// App exit: one last full write and a `savePlayQueue`, bounded.
    fn persist_final(&mut self) {
        let Some(session) = self.session.clone() else { return };
        if !self.tracking.enabled || !(self.tracking.activity || self.tracking.queue_dirty) {
            return;
        }
        let req = self.session_request(true, PushKind::Final);
        session.save_final(req, Duration::from_secs(3));
    }

    // --- Session restore -------------------------------------------------

    /// Rebuilds the queue from a saved session (or a server play queue) from
    /// the local library only — no network. Restored *paused* at the saved
    /// position: the stream opens when Play is pressed (`resume_or_start`).
    /// An empty library (first sync not done yet / just migrated) leaves the
    /// saved session untouched rather than judging every id deleted.
    fn restore_session(&mut self, state: SessionState, pools: Option<SourcePools>, force: bool) {
        if !force
            && (self.queue_touched
                || self.status != PlaybackStatus::Stopped
                || self.playback.is_some()
                || self.pending_start.is_some()
                || !self.queue.is_empty())
        {
            return;
        }
        // Look up only the ids the session actually names (queue entries, a static
        // queue's natural order, the rolling source's pool/remaining), not the whole tracks table.
        let mut wanted: std::collections::HashSet<&str> = state.queue.entries.iter().map(|e| e.id.as_str()).collect();
        wanted.extend(state.queue.static_natural_order.iter().map(String::as_str));
        if let Some(p) = &pools {
            wanted.extend(p.pool.iter().chain(&p.remaining).map(String::as_str));
        }
        let wanted: Vec<String> = wanted.into_iter().map(str::to_string).collect();
        let known = match library::track_artists_for_ids(&self.db, &wanted) {
            Ok(set) => set,
            Err(e) => {
                log::warn!("session restore: library read failed: {}", e.message);
                return;
            }
        };
        if known.is_empty() {
            log::info!("session restore skipped: none of the saved tracks is in the local library");
            return;
        }
        let from_server = state.volume.is_none();
        let old_repeat = self.queue.repeat;
        let mut restored = Queue::restore(
            state.queue,
            pools,
            &|id| known.contains_key(id),
            self.queue.window_size(),
            self.queue.spread_artists,
        );
        if restored.queue.current_id().is_none() {
            return; // nothing left to resume (queue had ended, or every id is gone)
        }
        // "Spread artists" data isn't persisted: re-derive it from the same
        // query, so a restored shuffle's later refills are spread too.
        restored.queue.fill_artists(&|id| known.get(id).cloned());
        drop(known);
        // Flow data isn't persisted either: without it a restored Flow source
        // would draw blind and lose its reasons.
        let flow_ids = restored.queue.flow_wanted_ids();
        if !flow_ids.is_empty() {
            match library::flow::load_flow_features(&self.db, &flow_ids) {
                Ok(f) => restored.queue.set_flow_features(f),
                Err(e) => log::warn!("session restore: flow data read failed: {}", e.message),
            }
        }
        if restored.dropped > 0 {
            log::info!("session restore: dropped {} unknown track(s)", restored.dropped);
        }
        self.consecutive_failures = 0;
        self.teardown_playback();
        self.cancel_pending_start();
        self.prefetched = None;
        self.queue = restored.queue;
        if from_server {
            self.queue.repeat = old_repeat;
        }
        if let Some(v) = state.volume.filter(|v| v.is_finite()) {
            self.volume = v.clamp(0.0, 1.0);
        }
        self.track_cache.clear();
        let current = self.queue.current_id().map(str::to_string);
        if let Some(id) = &current {
            self.resolve_tracks(std::slice::from_ref(id));
        }
        self.reset_autoplay();
        self.last_error = None;
        self.tracking.enabled = true;
        self.tracking.need_core = true;
        let position = if restored.current_kept { state.position_ms as f64 / 1000.0 } else { 0.0 };
        if force {
            // An explicit "Resume from server": adopt it as local activity.
            self.tracking.queue_dirty = true;
            self.start_current((position > 0.0).then_some(position));
        } else {
            // Startup restore: nothing is written until the user actually
            // does something, so the saved "last local activity" time (used
            // to decide whether the server's queue is newer) stays truthful.
            self.tracking.queue_dirty = false;
            self.tracking.last_current = self.queue.current_id().map(str::to_string);
            self.tracking.last_status = PlaybackStatus::Paused;
            self.resume_at = (position > 0.0).then_some(position);
            self.status = PlaybackStatus::Paused;
        }
    }
}

/// `PlayerState.floatMix`: the stream is the shared-mode mixer's 32-bit
/// float format. Not an exclusive float stream (a lossy source in exclusive
/// mode negotiates F32 too, but no mixer is involved there).
fn is_float_mix(n: &NegotiatedFormat) -> bool {
    n.mode == OutputMode::Shared && n.container == Container::F32
}

/// Id of the system default render endpoint (a single endpoint lookup).
fn real_default_device_id() -> Option<String> {
    output::default_device_id()
}

/// Can the next track (`next`) be appended to the current ring without
/// touching the device, bit-perfectly? Same rate and channels, and the
/// current container is one the next track may use (e.g. a 16-bit track
/// after a 24-bit one keeps the 24-in-32 stream, zero-padded).
fn can_continue(current: &NegotiatedFormat, next: &SourceFormat) -> bool {
    if current.sample_rate != next.sample_rate || current.channels != next.channels {
        return false;
    }
    match current.mode {
        OutputMode::Exclusive => next.candidate_containers().contains(&current.container),
        OutputMode::Shared => current.container == Container::F32,
    }
}

fn spawn_spectrum_thread(
    mut consumer: rtrb::Consumer<f32>,
    peaks: Arc<output::PeakMeter>,
    sample_rate: u32,
    events: Arc<dyn EngineEvents>,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
) {
    let spawned = std::thread::Builder::new()
        .name("sonix-spectrum".into())
        .spawn(move || {
            let mut analyzer = SpectrumAnalyzer::new(sample_rate);
            let mut scratch = vec![0f32; (sample_rate as usize / 10).max(256)];
            let mut dry_polls = 0u32;
            while !stop.load(Ordering::Relaxed) {
                if !active.load(Ordering::Relaxed) {
                    // Nobody is looking: the render side feeds nothing, so no
                    // FFT, no emit. Stale samples are dropped on resume.
                    let (_, _) = consumer.pop_partial_slice(&mut scratch);
                    dry_polls = SPECTRUM_DRY_POLLS;
                    std::thread::sleep(SPECTRUM_INACTIVE_TICK);
                    continue;
                }
                let (popped, _) = consumer.pop_partial_slice(&mut scratch);
                let popped_any = !popped.is_empty();
                if popped_any {
                    analyzer.push_samples(popped);
                    dry_polls = 0;
                } else {
                    dry_polls = dry_polls.saturating_add(1);
                }
                if let Some(bands) = analyzer.maybe_compute() {
                    let (pl, pr) = peaks.take();
                    events.spectrum(SpectrumFrame {
                        bands,
                        peak_l: peak_to_dbfs(pl),
                        peak_r: peak_to_dbfs(pr),
                    });
                }
                std::thread::sleep(if dry_polls >= SPECTRUM_DRY_POLLS { SPECTRUM_IDLE_TICK } else { SPECTRUM_TICK });
            }
        });
    if let Err(e) = spawned {
        log::warn!("spectrum thread not started: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_wait_is_the_earliest_deadline_within_bounds() {
        let ms = Duration::from_millis;
        // Fresh heartbeat, nothing dirty: the full heartbeat interval.
        assert_eq!(idle_wait(Some(ms(0)), None), IDLE_HEARTBEAT);
        // Heartbeat nearly due.
        assert_eq!(idle_wait(Some(ms(1500)), None), ms(500));
        // Overdue / never emitted: floor, not zero.
        assert_eq!(idle_wait(Some(ms(5000)), None), ms(20));
        assert_eq!(idle_wait(None, None), ms(20));
        // A debounced session write due sooner wins; later one does not.
        assert_eq!(idle_wait(Some(ms(0)), Some(ms(700))), ms(700));
        assert_eq!(idle_wait(Some(ms(1900)), Some(ms(700))), ms(100));
        assert_eq!(idle_wait(Some(ms(0)), Some(ms(0))), ms(20));
    }

    #[test]
    fn engine_is_idle_only_when_paused_or_stopped_with_nothing_pending() {
        let mut e = test_engine();
        assert!(e.is_idle(), "fresh engine is stopped and idle");
        e.status = PlaybackStatus::Playing;
        assert!(!e.is_idle());
        e.status = PlaybackStatus::Loading;
        assert!(!e.is_idle());
        e.status = PlaybackStatus::Paused;
        assert!(e.is_idle());
        e.autoplay.start_on_arrival = true;
        assert!(!e.is_idle());
    }

    #[test]
    fn state_emit_dedup_only_when_not_playing() {
        let idle = idle_player_state();
        let young = Duration::from_millis(250);
        assert!(should_emit_state(None, &idle));
        // Unchanged and not playing: skipped, until the heartbeat is due.
        assert!(!should_emit_state(Some((&idle, young)), &idle));
        assert!(should_emit_state(Some((&idle, IDLE_HEARTBEAT)), &idle));
        // Any change goes out at once.
        let mut moved = idle.clone();
        moved.status = PlaybackStatus::Paused;
        assert!(should_emit_state(Some((&idle, young)), &moved));
        // Playing always emits, even if identical.
        let mut playing = idle.clone();
        playing.status = PlaybackStatus::Playing;
        assert!(should_emit_state(Some((&playing, young)), &playing));
    }

    fn src(rate: u32, bits: Option<u8>) -> SourceFormat {
        SourceFormat {
            sample_rate: rate,
            channels: 2,
            bit_depth: bits,
            codec: "flac",
            total_frames: None,
        }
    }

    #[test]
    fn position_is_the_seek_target_until_the_render_thread_applies_the_seek() {
        use std::sync::atomic::{AtomicU32, AtomicU64};
        let (seek_tx, _seek_rx) = std::sync::mpsc::channel();
        let (_ev_tx, render_events) = std::sync::mpsc::channel();
        let played_frames = Arc::new(AtomicU64::new(0));
        let seeks_applied = Arc::new(AtomicU64::new(0));
        let render = crate::audio::output::RenderHandle {
            stop: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            played_frames: played_frames.clone(),
            gap_frames: Arc::new(AtomicU64::new(0)),
            volume_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            seek_tx,
            seeks_applied: seeks_applied.clone(),
            join: std::thread::spawn(|| {}),
        };
        let mut pb = Playback {
            negotiated: NegotiatedFormat::new(1_000, 2, Container::F32, OutputMode::Shared),
            render,
            render_events,
            spectrum_stop: Arc::new(AtomicBool::new(false)),
            device_name: None,
            opened_default: None,
            current: Slot { id: "t".into(), format: src(1_000, Some(16)), start_frame: 0, start_offset: 0.0 },
            feeder: None,
            pending: None,
            seeks_sent: 0,
        };
        played_frames.store(20_000, Ordering::Relaxed); // 20 s into the old ring
        assert!((pb.position_sec() - 20.0).abs() < 1e-9);
        // Seek to 100 s: the counter has not been reset by the render thread yet.
        pb.current.start_frame = 0;
        pb.current.start_offset = 100.0;
        pb.seeks_sent += 1;
        assert_eq!(pb.position_sec(), 100.0, "must not report 100 + 20 s of the old ring");
        // Render thread swaps rings: counter to 0, then the seek is applied.
        played_frames.store(0, Ordering::Relaxed);
        seeks_applied.fetch_add(1, Ordering::Release);
        played_frames.store(500, Ordering::Relaxed);
        assert!((pb.position_sec() - 100.5).abs() < 1e-9);
        pb.render.stop_and_join();
    }

    #[test]
    fn gapless_continuation_rules() {
        let excl24 = NegotiatedFormat::new(44_100, 2, Container::I24In32, OutputMode::Exclusive);
        assert!(can_continue(&excl24, &src(44_100, Some(24))));
        assert!(
            can_continue(&excl24, &src(44_100, Some(16))),
            "16-bit fits zero-padded in 24-in-32"
        );
        assert!(
            !can_continue(&excl24, &src(48_000, Some(24))),
            "rate change must reopen"
        );

        let excl16 = NegotiatedFormat::new(44_100, 2, Container::I16, OutputMode::Exclusive);
        assert!(
            !can_continue(&excl16, &src(44_100, Some(24))),
            "never truncate 24 -> 16"
        );

        let shared = NegotiatedFormat::new(44_100, 2, Container::F32, OutputMode::Shared);
        assert!(can_continue(&shared, &src(44_100, Some(24))));
        assert!(!can_continue(&shared, &src(96_000, Some(24))));
    }

    #[test]
    fn float_mix_is_shared_mode_only() {
        assert!(is_float_mix(&NegotiatedFormat::new(44_100, 2, Container::F32, OutputMode::Shared)));
        // A lossy source in exclusive mode negotiates a float stream, but no mixer.
        assert!(!is_float_mix(&NegotiatedFormat::new(44_100, 2, Container::F32, OutputMode::Exclusive)));
        assert!(!is_float_mix(&NegotiatedFormat::new(48_000, 2, Container::I16, OutputMode::Exclusive)));
    }

    // -----------------------------------------------------------------
    // Engine-level tests exercising real `EngineCommand` handling (the same
    // path the `player_*` commands drive) against an in-memory
    // library DB and a fake `TrackSource` — no real audio device or network
    // needed, so these run everywhere `cargo test` does. Device-dependent
    // behaviour (actual WASAPI playback, gapless hand-off, seeks) is covered
    // by `examples/engine_test.rs` against real hardware instead.
    // -----------------------------------------------------------------

    struct NullEvents;
    impl EngineEvents for NullEvents {
        fn player_state(&self, _state: &PlayerState) {}
        fn spectrum(&self, _frame: SpectrumFrame) {}
    }

    /// Always fails to open — used to exercise failure/recovery paths
    /// (`fail_current_track`, bounded skip-to-next-on-failure) without
    /// needing a real file or device.
    struct FailingSource;
    impl TrackSource for FailingSource {
        fn open(&self, _id: &str) -> std::io::Result<Box<dyn symphonia::core::io::MediaSource>> {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "test: no such track"))
        }
    }

    fn test_engine_with(source: Arc<dyn TrackSource>) -> Engine {
        let engine = Engine::new(
            Arc::new(NullEvents),
            source,
            Arc::new(Db::open_in_memory().unwrap()),
            false,
            super::super::queue::DEFAULT_WINDOW_SIZE as u32,
        );
        // `Prefs::default()` is autoplay OFF; these engine tests exercise autoplay.
        engine.prefs.set_autoplay(true);
        engine
    }

    fn test_engine() -> Engine {
        test_engine_with(Arc::new(FailingSource))
    }

    #[test]
    fn side_requests_never_start_the_engine_and_are_bounded() {
        let get = |reply| EngineCommand::GetQueue { offset: 0, limit: 10, reply };
        let h = EngineHandle::with_events(
            Arc::new(NullEvents),
            Arc::new(FailingSource),
            Arc::new(Db::open_in_memory().unwrap()),
            false,
            super::super::queue::DEFAULT_WINDOW_SIZE as u32,
        );
        assert_eq!(h.request_blocking_if_running(get, Duration::from_millis(50)).err(), Some(EngineRequestError::NotRunning));
        assert!(!h.is_running(), "a side read must not spawn the engine thread");
        // A running engine answers.
        assert!(h.request_blocking(get).is_some());
        let q = h.request_blocking_if_running(get, Duration::from_secs(2)).unwrap();
        assert_eq!(q.total, 0);
        // A stalled engine (commands accepted, never serviced): bounded, no hang.
        let (tx, _stalled_rx) = std::sync::mpsc::channel();
        *h.inner.lock().unwrap() = Some(tx);
        let started = Instant::now();
        assert_eq!(h.request_blocking_if_running(get, Duration::from_millis(100)).err(), Some(EngineRequestError::NoAnswer));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    fn seed_track(db: &Db, id: &str, title: &str) {
        db.with_tx(|tx| {
            tx.execute(
                "INSERT INTO tracks (id, title, artist, duration, suffix, sampling_rate, channel_count)
                 VALUES (?1, ?2, 'Test Artist', 200.0, 'flac', 44100, 2)",
                rusqlite::params![id, title],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn switching_to_shared_clears_a_stale_exclusive_refusal() {
        let mut engine = test_engine();
        engine.exclusive_error = Some("exclusive mode rejected at 44100 Hz/2 ch: AUDCLNT_E_UNSUPPORTED_FORMAT".into());
        engine.handle(EngineCommand::SetOutputMode(OutputMode::Shared));
        let status = engine.output_status();
        assert_eq!(status.mode, OutputMode::Shared);
        assert_eq!(status.exclusive_error, None);

        // Asking for exclusive again keeps whatever the next open decides
        // (nothing is playing here, so nothing is re-negotiated yet).
        engine.handle(EngineCommand::SetOutputMode(OutputMode::Exclusive));
        assert_eq!(engine.output_status().mode, OutputMode::Exclusive);
        assert_eq!(engine.output_status().exclusive_error, None);
    }

    /// A minimal but valid 16-bit PCM WAV `symphonia` can probe quickly.
    fn write_min_wav(path: &std::path::Path) {
        use std::io::Write;
        let (rate, bits, channels, frames): (u32, u16, u16, u32) = (44_100, 16, 2, 4096);
        let bps = (bits / 8) as u32;
        let data_len = frames * channels as u32 * bps;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&rate.to_le_bytes()).unwrap();
        f.write_all(&(rate * channels as u32 * bps).to_le_bytes()).unwrap();
        f.write_all(&((channels as u32 * bps) as u16).to_le_bytes()).unwrap();
        f.write_all(&bits.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_len.to_le_bytes()).unwrap();
        f.write_all(&vec![0u8; data_len as usize]).unwrap();
    }

    #[test]
    fn resolve_tracks_caches_real_metadata_and_falls_back_honestly_for_unknown_ids() {
        let mut engine = test_engine();
        seed_track(&engine.db, "tr1", "Opening Track");
        engine.resolve_tracks(&["tr1".to_string(), "unknown".to_string()]);

        let known = engine.track_for("tr1");
        assert_eq!(known.title, "Opening Track");
        assert_eq!(known.artist, "Test Artist");
        assert_eq!(known.format.suffix, "flac");

        // No local row and nothing decoded for it: an honestly-empty stub,
        // never fabricated metadata.
        let unknown = engine.track_for("unknown");
        assert_eq!(unknown.title, "unknown");
        assert_eq!(unknown.artist, "");
        assert_eq!(unknown.duration, 0.0);
    }

    /// Counts `open` (a new download) vs `reattach` (a reader over the
    /// download already running).
    struct ReuseSource {
        opens: std::sync::atomic::AtomicUsize,
        reattaches: std::sync::atomic::AtomicUsize,
        has_download: bool,
    }
    impl TrackSource for ReuseSource {
        fn open(&self, _id: &str) -> std::io::Result<Box<dyn symphonia::core::io::MediaSource>> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(std::io::Cursor::new(vec![0u8; 16])))
        }
        fn reattach(&self, _id: &str) -> Option<Box<dyn symphonia::core::io::MediaSource>> {
            self.reattaches.fetch_add(1, Ordering::SeqCst);
            self.has_download.then(|| Box::new(std::io::Cursor::new(vec![0u8; 16])) as Box<dyn symphonia::core::io::MediaSource>)
        }
    }

    #[test]
    fn output_restart_reuses_the_running_download_instead_of_fetching_again() {
        let source = Arc::new(ReuseSource {
            opens: Default::default(),
            reattaches: Default::default(),
            has_download: true,
        });
        let mut engine = test_engine_with(source.clone());
        engine.queue.set_queue(vec!["a".into(), "b".into()], 0);

        // What `restart_output` does before tearing the old playback down...
        engine.reuse_download_for_current();
        // ...and what `start_current` -> `spawn_probe` then asks for.
        engine.take_or_open("a").unwrap();
        assert_eq!(source.reattaches.load(Ordering::SeqCst), 1);
        assert_eq!(source.opens.load(Ordering::SeqCst), 0, "no new download for the same track");

        // Nothing live to reuse (e.g. a local file, or a failed download):
        // falls back to a normal open.
        let source = Arc::new(ReuseSource {
            opens: Default::default(),
            reattaches: Default::default(),
            has_download: false,
        });
        let mut engine = test_engine_with(source.clone());
        engine.queue.set_queue(vec!["a".into()], 0);
        engine.reuse_download_for_current();
        engine.take_or_open("a").unwrap();
        assert_eq!(source.opens.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn output_restart_keeps_the_next_tracks_prefetch() {
        // A device/mode switch mid-track: the current track restarts from its
        // running download and the already-running prefetch of the next track
        // survives (it used to be dropped and fetched again).
        let source = Arc::new(ReuseSource {
            opens: Default::default(),
            reattaches: Default::default(),
            has_download: true,
        });
        let mut engine = test_engine_with(source.clone());
        engine.queue.set_queue(vec!["a".into(), "b".into()], 0);
        engine.prefetched = Some(("b".into(), Box::new(std::io::Cursor::new(vec![0u8; 16]))));

        engine.restart_current_reusing_downloads(12.0, false);

        assert_eq!(source.reattaches.load(Ordering::SeqCst), 1);
        assert_eq!(source.opens.load(Ordering::SeqCst), 0, "neither track fetched again");
        assert_eq!(engine.pending_start.as_ref().map(|p| p.id.as_str()), Some("a"));
        assert_eq!(engine.prefetched.as_ref().map(|(id, _)| id.as_str()), Some("b"));
    }

    #[test]
    fn play_queue_resolves_metadata_before_the_decoder_responds() {
        // `start_current` never blocks (see `PendingStart`'s doc comment):
        // this test never calls `pump()`/`run()`, so the background probe it
        // spawns is never polled — `handle()` must still have returned with
        // real metadata and `Loading` status, proving resolution doesn't
        // wait on decode/device work.
        let dir = tempfile::tempdir().unwrap();
        let wav_path = dir.path().join("tr1.wav");
        write_min_wav(&wav_path);
        let id = wav_path.to_string_lossy().into_owned();

        let mut engine = test_engine_with(Arc::new(crate::audio::source::LocalFileSource));
        seed_track(&engine.db, &id, "Opening Track");

        engine.handle(EngineCommand::PlayQueue {
            track_ids: vec![id.clone()],
            start_index: 0,
        });

        assert_eq!(engine.status, PlaybackStatus::Loading);
        assert!(engine.pending_start.is_some(), "probe should be in flight, not awaited");
        let state = engine.player_state();
        assert_eq!(state.status, PlaybackStatus::Loading);
        assert_eq!(state.track.as_ref().map(|t| t.title.as_str()), Some("Opening Track"));
    }

    #[test]
    fn failed_opens_skip_through_the_whole_queue_and_stop_cleanly() {
        let mut engine = test_engine();
        engine.handle(EngineCommand::PlayQueue {
            track_ids: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            start_index: 0,
        });
        // `FailingSource::open` always errors, so `start_current` fails
        // synchronously for every track in turn (bounded recursion through
        // `fail_current_track` -> `advance` -> `start_current`); the engine
        // must land cleanly on `Stopped` with an empty queue position and a
        // recorded error, never hang or panic.
        assert_eq!(engine.status, PlaybackStatus::Stopped);
        assert!(engine.last_error.is_some());
        assert!(engine.queue.current_id().is_none());
    }

    #[test]
    fn repeat_all_with_failing_tracks_stops_instead_of_looping_forever() {
        let mut engine = test_engine();
        engine.handle(EngineCommand::SetRepeat(RepeatMode::All));
        engine.handle(EngineCommand::PlayQueue {
            track_ids: vec!["a".to_string(), "b".to_string()],
            start_index: 0,
        });
        assert_eq!(engine.status, PlaybackStatus::Stopped);
        assert_eq!(engine.consecutive_failures, MAX_CONSECUTIVE_FAILURES);
        assert!(engine.last_error.as_deref().unwrap_or("").contains("in a row"));
        assert!(engine.queue.current_id().is_some(), "queue position kept so Play can retry");
    }

    #[test]
    fn a_long_failing_queue_does_not_recurse_per_track() {
        let mut engine = test_engine();
        // Offline with a huge playlist: every open fails synchronously. Must
        // neither overflow the stack nor walk all 50k tracks.
        let ids: Vec<String> = (0..50_000).map(|i| format!("t{i}")).collect();
        engine.handle(EngineCommand::PlayQueue { track_ids: ids, start_index: 0 });
        assert_eq!(engine.status, PlaybackStatus::Stopped);
        assert_eq!(engine.queue.current_position(), Some(MAX_CONSECUTIVE_FAILURES as usize - 1));
    }

    #[test]
    fn track_cache_stays_bounded_over_a_long_rolling_window_session() {
        let mut engine = test_engine();
        let ids: Vec<String> = (0..600).map(|i| format!("tr{i}")).collect();
        for id in &ids {
            seed_track(&engine.db, id, id);
        }
        engine.queue.set_source(ipc::SourceKind::Playlist, "Big".into(), ipc::SourceMode::Shuffle, ids, vec![], None);
        for _ in 0..550 {
            let id = engine.queue.next().map(str::to_string).expect("pool not exhausted yet");
            engine.resolve_tracks(std::slice::from_ref(&id));
            engine.bound_track_cache();
        }
        assert!(
            engine.track_cache.len() <= engine.queue.len() + 128,
            "track_cache grew to {} entries for a {}-entry queue",
            engine.track_cache.len(),
            engine.queue.len()
        );
        let current = engine.queue.current_id().unwrap().to_string();
        assert!(engine.track_cache.contains_key(&current), "the current track's metadata is kept");
    }

    #[test]
    fn play_queue_drops_metadata_of_the_replaced_queue() {
        let mut engine = test_engine();
        seed_track(&engine.db, "tr1", "Opening Track");
        seed_track(&engine.db, "tr2", "Second Track");
        engine.handle(EngineCommand::PlayQueue {
            track_ids: vec!["tr1".to_string()],
            start_index: 0,
        });
        assert!(engine.track_cache.contains_key("tr1"));
        engine.handle(EngineCommand::PlayQueue {
            track_ids: vec!["tr2".to_string()],
            start_index: 0,
        });
        assert!(!engine.track_cache.contains_key("tr1"));
        assert!(engine.track_cache.contains_key("tr2"));
    }

    #[test]
    fn ab_loop_is_cleared_when_its_track_is_no_longer_current_and_ignored_without_playback() {
        let mut engine = test_engine();
        // Nothing audible: the button has no position to mark.
        engine.handle(EngineCommand::LoopCycle);
        assert_eq!(engine.player_state().loop_a, None);

        // A loop that belongs to a queue entry that is gone is dropped on the next tick.
        engine.ab_loop.cycle(3.0);
        engine.ab_loop.cycle(9.0);
        engine.loop_track = Some("old".into());
        assert!(engine.player_state().loop_b.is_none(), "never reported for another entry");
        engine.loop_tick();
        let st = engine.player_state();
        assert_eq!((st.loop_a, st.loop_b), (None, None));
        assert!(engine.loop_track.is_none());
    }

    #[test]
    fn stop_command_clears_queue_and_cached_metadata() {
        let mut engine = test_engine();
        seed_track(&engine.db, "tr1", "Opening Track");
        engine.handle(EngineCommand::PlayQueue {
            track_ids: vec!["tr1".to_string()],
            start_index: 0,
        });
        assert!(!engine.track_cache.is_empty());

        engine.handle(EngineCommand::Stop);
        assert_eq!(engine.status, PlaybackStatus::Stopped);
        assert!(engine.queue.current_id().is_none());
        assert!(engine.track_cache.is_empty());
        assert!(engine.last_error.is_none());
    }

    // -----------------------------------------------------------------
    // Continuity: ReplayGain, session restore/persist, autoplay
    // -----------------------------------------------------------------

    fn seed_track_rg(db: &Db, id: &str, track_gain: Option<f64>, album_gain: Option<f64>, track_peak: Option<f64>) {
        db.with_tx(|tx| {
            tx.execute(
                "INSERT INTO tracks (id, title, artist, duration, suffix, sampling_rate, channel_count,
                                     track_gain_db, album_gain_db, track_peak)
                 VALUES (?1, ?1, 'Test Artist', 200.0, 'flac', 44100, 2, ?2, ?3, ?4)",
                rusqlite::params![id, track_gain, album_gain, track_peak],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn replay_gain_off_missing_tags_track_album_and_peak_clamp() {
        let mut engine = test_engine();
        seed_track_rg(&engine.db, "tagged", Some(-6.0), Some(-3.0), None);
        seed_track_rg(&engine.db, "hot", Some(6.0), None, Some(0.8));
        seed_track(&engine.db, "untagged", "No tags");
        engine.resolve_tracks(&["tagged".into(), "hot".into(), "untagged".into()]);

        // Default (off): unity everywhere, nothing reported.
        assert_eq!(engine.effective_gain_for(Some("tagged")), 1.0);
        assert!(engine.replay_gain_for("tagged").is_none());

        engine.prefs.set_replay_gain(ReplayGainMode::Track);
        let g = engine.effective_gain_for(Some("tagged"));
        assert!((g - 0.501_187).abs() < 1e-4, "track gain -6 dB, got {g}");
        assert_eq!(engine.effective_gain_for(Some("untagged")), 1.0, "missing tags are honestly unity");
        let hot = engine.effective_gain_for(Some("hot"));
        assert!((hot - 1.25).abs() < 1e-5, "+6 dB clamped to 1/peak, got {hot}");

        engine.prefs.set_replay_gain(ReplayGainMode::Album);
        let g = engine.effective_gain_for(Some("tagged"));
        assert!((g - 0.707_946).abs() < 1e-4, "album gain -3 dB, got {g}");
    }

    #[test]
    fn replay_gain_is_combined_with_volume_and_breaks_bit_perfect_unless_unity() {
        let mut engine = test_engine();
        seed_track_rg(&engine.db, "tagged", Some(-6.0), None, None);
        seed_track_rg(&engine.db, "zero", Some(0.0), None, None);
        seed_track(&engine.db, "untagged", "No tags");
        engine.resolve_tracks(&["tagged".into(), "zero".into(), "untagged".into()]);
        engine.prefs.set_replay_gain(ReplayGainMode::Track);

        // Volume at the top, RG active and non-zero: not unity -> not bit-perfect.
        assert!(!gain_is_unity(engine.effective_gain_for(Some("tagged"))));
        // A 0 dB tag, a missing tag, or the mode off: unity, bit-perfect possible.
        assert!(gain_is_unity(engine.effective_gain_for(Some("zero"))));
        assert!(gain_is_unity(engine.effective_gain_for(Some("untagged"))));
        engine.prefs.set_replay_gain(ReplayGainMode::Off);
        assert!(gain_is_unity(engine.effective_gain_for(Some("tagged"))));

        // User volume below the top is multiplied in (-30 dB at 0.5, then RG -6 dB).
        engine.prefs.set_replay_gain(ReplayGainMode::Track);
        engine.volume = 0.5;
        let g = engine.effective_gain_for(Some("tagged"));
        let expected = output::volume_to_gain(0.5) * 0.501_187;
        assert!((g - expected).abs() < 1e-6);
        assert!(!gain_is_unity(g));
    }

    fn state_with(ids: &[&str], current: usize, position_ms: u64, volume: Option<f32>) -> SessionState {
        use crate::audio::queue::{EntrySnapshot, QueueCore};
        SessionState {
            version: crate::audio::session::SESSION_VERSION,
            saved_at_ms: 1,
            position_ms,
            volume,
            queue: QueueCore {
                entries: ids
                    .iter()
                    .map(|id| EntrySnapshot { id: id.to_string(), origin: ipc::QueueItemOrigin::Source })
                    .collect(),
                current: Some(current),
                shuffle: false,
                repeat: RepeatMode::Off,
                static_natural_order: ids.iter().map(|s| s.to_string()).collect(),
                source: None,
            },
        }
    }

    #[test]
    fn restore_comes_back_paused_at_the_saved_position_from_the_local_library_only() {
        let mut engine = test_engine();
        for id in ["a", "b", "c"] {
            seed_track(&engine.db, id, &format!("Track {id}"));
        }
        // "gone" was deleted from the library since the session was saved.
        engine.handle(EngineCommand::RestoreSession {
            state: Box::new(state_with(&["a", "gone", "b", "c"], 2, 93_500, Some(0.75))),
            pools: None,
            force: false,
        });
        assert_eq!(engine.status, PlaybackStatus::Paused);
        assert!(engine.playback.is_none() && engine.pending_start.is_none(), "no stream is opened on restore");
        assert_eq!(engine.queue.ordered_ids(), vec!["a", "b", "c"], "unknown ids dropped");
        let state = engine.player_state();
        assert_eq!(state.status, PlaybackStatus::Paused);
        assert_eq!(state.track.as_ref().map(|t| t.title.as_str()), Some("Track b"));
        assert!((state.position_sec - 93.5).abs() < 1e-9);
        assert!((state.volume - 0.75).abs() < 1e-6);
        assert!(engine.tracking.enabled);
        assert!(!engine.tracking.queue_dirty, "restoring alone must not rewrite the session");
    }

    #[test]
    fn restore_resets_the_position_when_the_current_track_was_deleted() {
        let mut engine = test_engine();
        seed_track(&engine.db, "a", "A");
        seed_track(&engine.db, "c", "C");
        engine.handle(EngineCommand::RestoreSession {
            state: Box::new(state_with(&["a", "b", "c"], 1, 50_000, Some(1.0))),
            pools: None,
            force: false,
        });
        assert_eq!(engine.queue.current_id(), Some("c"));
        assert_eq!(engine.position_sec(), 0.0, "a position in a deleted track must not be applied to its neighbour");
    }

    #[test]
    fn restore_is_skipped_for_an_empty_library_or_when_the_user_already_queued_something() {
        let mut engine = test_engine();
        engine.handle(EngineCommand::RestoreSession { state: Box::new(state_with(&["a"], 0, 1_000, Some(1.0))), pools: None, force: false });
        assert!(engine.queue.is_empty(), "empty library (first sync pending) restores nothing");
        assert!(!engine.tracking.enabled, "and must not start overwriting the saved session");

        seed_track(&engine.db, "a", "A");
        seed_track(&engine.db, "x", "X");
        engine.queue.set_queue(vec!["x".into()], 0);
        engine.handle(EngineCommand::RestoreSession { state: Box::new(state_with(&["a"], 0, 1_000, Some(1.0))), pools: None, force: false });
        assert_eq!(engine.queue.ordered_ids(), vec!["x"], "a startup restore never replaces what the user is doing");
        // An explicit server resume does replace it (force), keeping the local repeat mode.
        engine.queue.set_repeat(RepeatMode::All);
        engine.handle(EngineCommand::RestoreSession { state: Box::new(state_with(&["a"], 0, 1_000, None)), pools: None, force: true });
        assert_eq!(engine.queue.current_id(), Some("a"));
        assert_eq!(engine.queue.repeat, RepeatMode::All);
    }

    #[test]
    fn session_is_persisted_from_the_engine_and_idle_engines_write_nothing() {
        let dir = std::env::temp_dir().join(format!("sonix-engine-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = SessionStore::spawn(dir.clone(), Arc::new(std::sync::RwLock::new(None)), Arc::new(Prefs::default()));
        let media = tempfile::tempdir().unwrap();
        let ids: Vec<String> = ["a.wav", "b.wav"]
            .iter()
            .map(|n| {
                let p = media.path().join(n);
                write_min_wav(&p);
                p.to_string_lossy().into_owned()
            })
            .collect();
        let mut engine = test_engine_with(Arc::new(crate::audio::source::LocalFileSource));
        engine.session = Some(store);

        // Idle: nothing to save, even at exit.
        engine.persist_tick();
        engine.persist_final();
        assert!(crate::audio::session::load(&dir).is_none());

        seed_track(&engine.db, &ids[0], "A");
        seed_track(&engine.db, &ids[1], "B");
        engine.handle(EngineCommand::PlayQueue { track_ids: ids.clone(), start_index: 1 });
        assert_eq!(engine.queue.current_id(), Some(ids[1].as_str()));
        engine.tracking.queue_dirty = true;
        engine.tracking.last_write = Instant::now() - Duration::from_secs(5);
        engine.persist_tick();
        engine.persist_final(); // blocks until the writer has written
        let (saved, _) = crate::audio::session::load(&dir).expect("session written");
        assert_eq!(saved.current_id(), Some(ids[1].as_str()));
        assert_eq!(saved.queue.entries.len(), 2);
        assert_eq!(saved.volume, Some(1.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logout_stop_clears_the_saved_session_and_autoplay_state() {
        let dir = std::env::temp_dir().join(format!("sonix-engine-session-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = SessionStore::spawn(dir.clone(), Arc::new(std::sync::RwLock::new(None)), Arc::new(Prefs::default()));
        let mut engine = test_engine();
        engine.session = Some(store.clone());
        seed_track(&engine.db, "a", "A");
        engine.handle(EngineCommand::PlayQueue { track_ids: vec!["a".into()], start_index: 0 });
        engine.persist_tick(); // notices the queue and enables persistence
        engine.persist_final();
        assert!(crate::audio::session::load(&dir).is_some());
        engine.handle(EngineCommand::Stop);
        // `clear` is ordered before this flush on the same channel.
        assert!(store.save_final(
            SaveRequest { queue: None, pools: None, position_ms: 0, volume: 1.0, push: PushKind::None },
            Duration::from_secs(5)
        ));
        assert!(crate::audio::session::load(&dir).is_none());
        assert!(!engine.tracking.enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn at_last_track(engine: &mut Engine, ids: &[&str]) {
        for id in ids {
            seed_track(&engine.db, id, &format!("Title {id}"));
        }
        engine.queue.set_queue(ids.iter().map(|s| s.to_string()).collect(), ids.len() - 1);
        let owned: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        engine.resolve_tracks(&owned);
        assert!(engine.queue.is_at_end());
    }

    #[test]
    fn autoplay_appends_a_radio_source_behind_the_last_track_without_recent_or_queued_ids() {
        let mut engine = test_engine();
        at_last_track(&mut engine, &["t0", "t1", "t2"]);
        engine.recent_played.push_back("r1".into());
        engine.apply_autoplay(
            "t2",
            vec![
                ("r1".into(), None),       // played recently
                ("t0".into(), None),       // already in the queue
                ("r2".into(), Some("A".into())),
                ("r3".into(), None),
            ],
        );
        assert_eq!(engine.queue.current_id(), Some("t2"), "the playing track is untouched");
        let (kind, label, mode, _, _) = engine.queue.source_info().expect("radio source");
        assert_eq!(kind, ipc::SourceKind::Radio);
        assert_eq!(label, "Autoplay · Title t2");
        assert_eq!(mode, ipc::SourceMode::InOrder);
        assert!(!engine.queue.is_at_end());
        assert_eq!(engine.queue.next(), Some("r2"));
        assert_eq!(engine.queue.next(), Some("r3"));
        assert!(engine.queue.is_at_end());
    }

    #[test]
    fn autoplay_with_no_usable_candidates_leaves_the_queue_to_end_and_is_not_retried() {
        let mut engine = test_engine();
        at_last_track(&mut engine, &["t0", "t1"]);
        engine.autoplay.requested_for = Some("t1".into());
        engine.queue.next(); // the queue ran out while the lookup was running
        engine.autoplay.start_on_arrival = true;
        engine.status = PlaybackStatus::Loading;
        engine.apply_autoplay("t1", vec![("t0".into(), None)]);
        assert_eq!(engine.status, PlaybackStatus::Stopped, "a waiting end-of-queue just stops");
        assert!(engine.queue.source_info().is_none());
        assert!(!engine.autoplay.start_on_arrival);
        assert_eq!(engine.autoplay.requested_for.as_deref(), Some("t1"), "the dead end is remembered: no request loop");
    }

    #[test]
    fn autoplay_answer_for_a_track_the_user_already_left_is_discarded() {
        let mut engine = test_engine();
        at_last_track(&mut engine, &["t0", "t1"]);
        engine.queue.previous(); // the user went back to t0
        engine.apply_autoplay("t1", vec![("r1".into(), None)]);
        assert!(engine.queue.source_info().is_none());
        assert_eq!(engine.queue.current_id(), Some("t0"));
    }

    #[test]
    fn autoplay_lookup_result_is_polled_from_the_helper_thread_channel() {
        let mut engine = test_engine();
        at_last_track(&mut engine, &["t0", "t1"]);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(vec![("r1".to_string(), None)]).unwrap();
        engine.autoplay.inflight = Some(InflightAutoplay { seed: "t1".into(), rx });
        engine.poll_autoplay();
        assert!(engine.autoplay.inflight.is_none());
        assert_eq!(engine.queue.source_info().map(|s| s.0), Some(ipc::SourceKind::Radio));
    }

    #[test]
    fn stop_and_new_queues_reset_autoplay_so_an_explicit_stop_never_autoplays() {
        let mut engine = test_engine();
        at_last_track(&mut engine, &["t0", "t1"]);
        engine.autoplay.requested_for = Some("t1".into());
        engine.autoplay.start_on_arrival = true;
        engine.handle(EngineCommand::Stop);
        assert!(engine.autoplay.requested_for.is_none() && !engine.autoplay.start_on_arrival);
        assert!(engine.queue.is_empty(), "nothing left to be at the end of");
        assert!(!engine.queue.is_at_end());
    }

    #[test]
    fn autoplay_answer_is_dropped_when_the_queue_no_longer_ends_on_the_seed() {
        // The user enqueued a track while the lookup ran: the seed is still
        // current but no longer last, so the radio must not be appended.
        let mut engine = test_engine();
        at_last_track(&mut engine, &["t0", "t1"]);
        seed_track(&engine.db, "u1", "User pick");
        engine.handle(EngineCommand::Enqueue { track_ids: vec!["u1".into()], position: EnqueuePosition::End });
        engine.apply_autoplay("t1", vec![("r1".into(), None)]);
        assert!(engine.queue.source_info().is_none());
        // Same for repeat turned on while the lookup ran.
        let mut engine = test_engine();
        at_last_track(&mut engine, &["t0", "t1"]);
        engine.handle(EngineCommand::SetRepeat(RepeatMode::All));
        engine.apply_autoplay("t1", vec![("r1".into(), None)]);
        assert!(engine.queue.source_info().is_none());
    }

    #[test]
    fn autoplay_keeps_the_users_shuffle_setting() {
        let mut engine = test_engine();
        engine.queue.set_shuffle(true);
        at_last_track(&mut engine, &["t0"]);
        engine.apply_autoplay("t0", vec![("r1".into(), None), ("r2".into(), None)]);
        assert!(engine.queue.shuffle, "autoplay doesn't flip the global shuffle mode");
        assert_eq!(engine.queue.source_info().map(|s| s.2), Some(ipc::SourceMode::InOrder));
    }

    #[test]
    fn a_late_autoplay_answer_never_clobbers_something_the_user_started_meanwhile() {
        let mut engine = test_engine();
        at_last_track(&mut engine, &["t0", "t1"]);
        // The queue ran out while the lookup was running...
        engine.queue.next();
        engine.autoplay.start_on_arrival = true;
        engine.status = PlaybackStatus::Loading;
        // ...then the user jumped back into the queue (now loading/playing t0).
        assert!(engine.queue.jump_to(0));
        engine.status = PlaybackStatus::Playing;
        engine.apply_autoplay("t1", vec![("r1".into(), None)]);
        assert_eq!(engine.status, PlaybackStatus::Playing, "status untouched");
        assert_eq!(engine.queue.current_id(), Some("t0"));
        assert!(engine.queue.source_info().is_none());
    }

    #[test]
    fn pausing_while_waiting_for_autoplay_cancels_the_start_on_arrival() {
        let mut engine = test_engine();
        at_last_track(&mut engine, &["t0", "t1"]);
        engine.queue.next();
        engine.autoplay.start_on_arrival = true;
        engine.status = PlaybackStatus::Loading;
        engine.handle(EngineCommand::PlayPause);
        assert!(!engine.autoplay.start_on_arrival);
        assert_eq!(engine.status, PlaybackStatus::Stopped);
        engine.apply_autoplay("t1", vec![("r1".into(), None)]);
        assert_eq!(engine.status, PlaybackStatus::Stopped);
        assert!(engine.pending_start.is_none() && engine.playback.is_none(), "nothing was started");
    }

    #[test]
    fn restore_rederives_spread_artists_for_a_restored_source() {
        let mut engine = test_engine();
        for i in 0..30 {
            seed_track(&engine.db, &format!("p{i}"), &format!("P{i}"));
        }
        let mut q = Queue::new();
        q.set_source(ipc::SourceKind::Playlist, "P".into(), ipc::SourceMode::Shuffle, (0..30).map(|i| format!("p{i}")).collect(), vec![], None);
        let state = SessionState {
            version: crate::audio::session::SESSION_VERSION,
            saved_at_ms: 1,
            position_ms: 0,
            volume: Some(1.0),
            queue: q.snapshot_core(),
        };
        engine.handle(EngineCommand::RestoreSession { state: Box::new(state), pools: q.snapshot_pools(), force: false });
        assert!(engine.queue.source_info().is_some(), "source restored");
        assert_eq!(engine.queue.artists_known(), 30, "every pool id got its artist from the restore's query");
    }

    #[test]
    fn flow_play_source_loads_features_reports_reasons_and_coverage_and_survives_a_restore() {
        let mut engine = test_engine();
        for i in 0..30 {
            seed_track(&engine.db, &format!("f{i}"), &format!("F{i}"));
        }
        engine
            .db
            .with_tx(|tx| {
                for i in (0..30).step_by(2) {
                    tx.execute(
                        "INSERT INTO track_features (track_id, bpm, source, updated_at) VALUES (?1, ?2, 'tag', 1)",
                        rusqlite::params![format!("f{i}"), 100.0 + i as f64],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let ids: Vec<String> = (0..30).map(|i| format!("f{i}")).collect();
        engine.queue.shuffle = true; // starting Flow must switch the global flag off (like In order)
        engine.handle(EngineCommand::PlaySource {
            kind: ipc::SourceKind::Playlist,
            label: "F".into(),
            source_id: None,
            mode: ipc::SourceMode::Flow,
            arc: ipc::FlowArc::Build,
            track_ids: ids,
            artists: vec![],
            start_track_id: None,
        });
        assert!(!engine.queue.shuffle);
        let get_queue = |engine: &mut Engine| {
            let (tx, mut rx) = tokio::sync::oneshot::channel();
            engine.handle(EngineCommand::GetQueue { offset: 0, limit: 100, reply: tx });
            rx.try_recv().expect("GetQueue replies synchronously")
        };
        let q = get_queue(&mut engine);
        let src = q.source.expect("source info");
        assert_eq!(src.mode, ipc::SourceMode::Flow);
        assert_eq!(src.arc, Some(ipc::FlowArc::Build));
        let cov = src.flow_coverage.expect("coverage");
        assert_eq!((cov.total, cov.bpm, cov.key), (30, 15, 0));
        let with_bpm = q.items.iter().filter(|it| it.flow.as_ref().is_some_and(|f| f.bpm_from.is_some())).count();
        assert!(with_bpm > 0, "some row explains itself with real BPM numbers");
        assert!(q.items.iter().all(|it| it.flow.as_ref().is_none_or(|f| f.key_from.is_none())), "no key anywhere, so none reported");

        // Restore: the data is not persisted, the engine re-reads it from the library.
        let state = SessionState {
            version: crate::audio::session::SESSION_VERSION,
            saved_at_ms: 1,
            position_ms: 0,
            volume: Some(1.0),
            queue: engine.queue.snapshot_core(),
        };
        let pools = engine.queue.snapshot_pools();
        engine.queue = Queue::new();
        engine.queue_touched = false; // a fresh engine (next app start): the startup restore applies
        engine.handle(EngineCommand::RestoreSession { state: Box::new(state), pools, force: false });
        let (arc, cov) = engine.queue.flow_info().expect("restored Flow source");
        assert_eq!(arc, ipc::FlowArc::Build);
        assert_eq!(cov.bpm, 15, "features re-read after the restore");
        let q = get_queue(&mut engine);
        assert!(q.items.iter().any(|it| it.flow.is_some()), "reasons are back after the restore");
    }

    // --- skip without reopening the device ------------------------------

    struct FakeOutput {
        pb: Playback,
        seek_rx: Receiver<rtrb::Consumer<u8>>,
        stop: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
    }

    fn fake_output(negotiated: NegotiatedFormat, current: &str) -> FakeOutput {
        use std::sync::atomic::{AtomicU32, AtomicU64};
        let (seek_tx, seek_rx) = std::sync::mpsc::channel();
        let (_ev_tx, render_events) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let render = RenderHandle {
            stop: stop.clone(),
            paused: paused.clone(),
            played_frames: Arc::new(AtomicU64::new(0)),
            gap_frames: Arc::new(AtomicU64::new(0)),
            volume_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            seek_tx,
            seeks_applied: Arc::new(AtomicU64::new(0)),
            join: std::thread::spawn(|| {}),
        };
        let pb = Playback {
            negotiated,
            render,
            render_events,
            spectrum_stop: Arc::new(AtomicBool::new(false)),
            device_name: None,
            opened_default: None,
            current: Slot { id: current.into(), format: src(44_100, Some(16)), start_frame: 0, start_offset: 0.0 },
            feeder: None,
            pending: None,
            seeks_sent: 0,
        };
        FakeOutput { pb, seek_rx, stop, paused }
    }

    struct SkipFixture {
        engine: Engine,
        seek_rx: Receiver<rtrb::Consumer<u8>>,
        stop: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        old_cmds: Receiver<DecodeCommand>,
        ids: [String; 2],
        _dir: tempfile::TempDir,
    }

    /// Engine over two real (tiny) WAV files, a and b, with a fake running
    /// output (exclusive 16-bit 44.1 kHz) playing a.
    fn skip_fixture() -> SkipFixture {
        let dir = tempfile::tempdir().unwrap();
        let mut ids = Vec::new();
        for name in ["a.wav", "b.wav"] {
            let path = dir.path().join(name);
            write_min_wav(&path);
            ids.push(path.to_string_lossy().into_owned());
        }
        let mut engine = test_engine_with(Arc::new(crate::audio::source::LocalFileSource));
        seed_track(&engine.db, &ids[0], "A");
        seed_track(&engine.db, &ids[1], "B");
        engine.handle(EngineCommand::PlayQueue { track_ids: ids.clone(), start_index: 0 });
        engine.cancel_pending_start(); // we install the "already playing" state by hand
        let mut out = fake_output(
            NegotiatedFormat::new(44_100, 2, Container::I16, OutputMode::Exclusive),
            &ids[0],
        );
        let (cmd_tx, old_cmds) = std::sync::mpsc::channel();
        let (_evt_tx, evt_rx) = std::sync::mpsc::channel();
        out.pb.feeder = Some(Feeder { commands: cmd_tx, events: evt_rx });
        engine.playback = Some(out.pb);
        engine.status = PlaybackStatus::Playing;
        SkipFixture {
            engine,
            seek_rx: out.seek_rx,
            stop: out.stop,
            paused: out.paused,
            old_cmds,
            ids: [ids[0].clone(), ids[1].clone()],
            _dir: dir,
        }
    }

    fn pump_until_started(engine: &mut Engine) {
        let t = Instant::now();
        while engine.pending_start.is_some() && t.elapsed() < Duration::from_secs(10) {
            engine.pump();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(engine.pending_start.is_none(), "probe never answered");
    }

    #[test]
    fn next_with_a_compatible_format_keeps_the_device_open() {
        let mut f = skip_fixture();
        f.engine.handle(EngineCommand::Next);
        // Silenced at once: the old decoder is stopped, the render thread got
        // an empty ring, and the output is parked, not closed.
        assert!(f.engine.playback.is_none() && f.engine.parked.is_some());
        assert!(matches!(f.old_cmds.try_recv(), Ok(DecodeCommand::Stop)));
        assert!(f.seek_rx.try_recv().is_ok(), "silent ring handed to the render thread");
        assert_eq!(f.engine.status, PlaybackStatus::Loading);

        pump_until_started(&mut f.engine);
        assert!(f.engine.parked.is_none());
        let pb = f.engine.playback.as_ref().expect("playing on the parked output");
        assert!(!f.stop.load(Ordering::Relaxed), "render thread was never stopped (no reopen)");
        assert!(f.seek_rx.try_recv().is_ok(), "the new track's ring arrived via the seek channel");
        assert_eq!(pb.current.id, f.ids[1]);
        assert_eq!(pb.current.start_frame, 0);
        assert_eq!(pb.seeks_sent, 2);
        assert!(pb.feeder.is_some() && pb.pending.is_none());
        assert!(!pb.render.paused.load(Ordering::Relaxed));
        assert_eq!(f.engine.status, PlaybackStatus::Playing);
        let state = f.engine.player_state();
        assert_eq!(state.track.as_ref().map(|t| t.title.as_str()), Some("B"));
        assert_eq!(state.position_sec, 0.0);
        f.engine.teardown_playback();
    }

    #[test]
    fn jump_to_and_removing_the_current_track_also_switch_in_place() {
        let mut f = skip_fixture();
        f.engine.handle(EngineCommand::JumpTo(1));
        assert!(f.engine.parked.is_some());
        pump_until_started(&mut f.engine);
        assert_eq!(f.engine.playback.as_ref().map(|p| p.current.id.clone()), Some(f.ids[1].clone()));
        assert!(!f.stop.load(Ordering::Relaxed));

        // Remove-current parks again (the queue then moves on).
        f.engine.handle(EngineCommand::RemoveFromQueue(1));
        assert!(f.engine.playback.is_none());
        f.engine.teardown_playback();
        assert!(f.engine.parked.is_none());
    }

    #[test]
    fn pause_while_the_skipped_to_track_loads_lands_paused() {
        let mut f = skip_fixture();
        f.engine.handle(EngineCommand::Next);
        f.engine.handle(EngineCommand::Pause);
        assert_eq!(f.engine.status, PlaybackStatus::Paused);
        pump_until_started(&mut f.engine);
        assert_eq!(f.engine.status, PlaybackStatus::Paused);
        assert!(f.paused.load(Ordering::Relaxed), "device stays silent");
        assert!(!f.stop.load(Ordering::Relaxed));
        f.engine.teardown_playback();
    }

    #[test]
    fn seek_while_the_skipped_to_track_loads_sets_the_start_offset() {
        let mut f = skip_fixture();
        f.engine.handle(EngineCommand::Next);
        f.engine.handle(EngineCommand::Seek(0.5));
        pump_until_started(&mut f.engine);
        let pb = f.engine.playback.as_ref().unwrap();
        assert_eq!(pb.current.start_offset, 0.5);
        assert_eq!(pb.position_sec(), 0.5);
        f.engine.teardown_playback();
    }

    #[test]
    fn skip_during_a_prefetched_gapless_tail_drops_the_pending_hand_off() {
        let mut f = skip_fixture();
        let (next_tx, next_cmds) = std::sync::mpsc::channel();
        let (_e, next_events) = std::sync::mpsc::channel();
        let ids = f.ids.clone();
        let pb = f.engine.playback.as_mut().unwrap();
        pb.feeder = None; // the audible decoder already finished
        pb.pending = Some(Pending {
            boundary: 10,
            next: Some(Slot { id: ids[1].clone(), format: src(48_000, Some(16)), start_frame: 0, start_offset: 0.0 }),
            reopen: Some(Feeder { commands: next_tx, events: next_events }),
            queue_after: None,
        });
        f.engine.handle(EngineCommand::Next);
        assert!(matches!(next_cmds.try_recv(), Ok(DecodeCommand::Stop)), "prefetched next decoder stopped");
        assert!(f.engine.parked.as_ref().unwrap().pending.is_none());
        pump_until_started(&mut f.engine);
        let pb = f.engine.playback.as_ref().unwrap();
        assert!(pb.pending.is_none());
        assert_eq!(pb.current.id, ids[1]);
        assert!(!f.stop.load(Ordering::Relaxed));
        f.engine.teardown_playback();
    }

    #[test]
    fn an_incompatible_format_or_mode_does_not_reuse_the_parked_output() {
        let engine = test_engine();
        let excl16 = fake_output(NegotiatedFormat::new(44_100, 2, Container::I16, OutputMode::Exclusive), "x");
        assert!(engine.can_switch_in_place(&excl16.pb, &src(44_100, Some(16))));
        assert!(!engine.can_switch_in_place(&excl16.pb, &src(48_000, Some(16))), "rate change reopens");
        assert!(!engine.can_switch_in_place(&excl16.pb, &src(44_100, Some(24))), "never truncates 24 to 16");
        // A shared stream is not a stand-in for a requested exclusive one.
        let shared = fake_output(NegotiatedFormat::new(44_100, 2, Container::F32, OutputMode::Shared), "x");
        assert!(!engine.can_switch_in_place(&shared.pb, &src(44_100, Some(16))));
        let mut shared_engine = test_engine();
        shared_engine.output_mode_pref = OutputMode::Shared;
        assert!(shared_engine.can_switch_in_place(&shared.pb, &src(44_100, Some(16))));
        excl16.pb.shutdown();
        shared.pb.shutdown();
    }

    #[test]
    fn an_output_change_drops_the_parked_output() {
        let mut f = skip_fixture();
        f.engine.handle(EngineCommand::Next);
        assert!(f.engine.parked.is_some());
        f.engine.handle(EngineCommand::SetOutputMode(OutputMode::Shared));
        assert!(f.engine.parked.is_none());
        assert!(f.stop.load(Ordering::Relaxed), "parked render thread stopped");
        f.engine.cancel_pending_start();
    }

    #[test]
    fn skipping_off_the_end_of_the_queue_closes_the_parked_output() {
        let mut f = skip_fixture();
        f.engine.handle(EngineCommand::Next); // b
        pump_until_started(&mut f.engine);
        f.engine.handle(EngineCommand::Next); // past the end
        assert!(f.engine.parked.is_none() && f.engine.playback.is_none());
        assert!(f.stop.load(Ordering::Relaxed));
        assert_eq!(f.engine.status, PlaybackStatus::Stopped);
    }

    #[test]
    fn a_moved_system_default_device_forces_a_reopen_on_skip() {
        fn dev_a() -> Option<String> { Some("A".into()) }
        fn dev_b() -> Option<String> { Some("B".into()) }
        fn dev_none() -> Option<String> { None }
        let mut engine = test_engine();
        let mut out = fake_output(NegotiatedFormat::new(44_100, 2, Container::I16, OutputMode::Exclusive), "x");
        out.pb.opened_default = Some("A".into());
        engine.default_id_lookup = dev_b;
        assert!(!engine.can_switch_in_place(&out.pb, &src(44_100, Some(16))), "default moved A -> B");
        engine.default_id_lookup = dev_a;
        assert!(engine.can_switch_in_place(&out.pb, &src(44_100, Some(16))), "default unchanged");
        engine.default_id_lookup = dev_none;
        assert!(engine.can_switch_in_place(&out.pb, &src(44_100, Some(16))), "unknown default keeps the output");
        // A chosen device never follows the default.
        engine.default_id_lookup = dev_b;
        engine.output_device_id = Some("A".into());
        assert!(engine.can_switch_in_place(&out.pb, &src(44_100, Some(16))));
        out.pb.shutdown();
    }

    #[test]
    fn no_boundary_commit_while_a_ring_swap_is_unapplied() {
        let mut f = skip_fixture();
        let pb = f.engine.playback.as_mut().unwrap();
        pb.render.played_frames.store(1_000_000, Ordering::Relaxed); // old ring's count
        pb.seeks_sent = 1; // render thread has not applied it
        pb.pending = Some(Pending { boundary: 10, next: None, reopen: None, queue_after: None });
        f.engine.check_boundary();
        let pb = f.engine.playback.as_ref().expect("no commit, playback kept");
        assert!(pb.pending.is_some());
        f.engine.playback.as_mut().unwrap().render.seeks_applied.store(1, Ordering::Release);
        f.engine.teardown_playback();
    }

    // --- listen events: engine-side reason plumbing ----------------------------

    /// Engine with a recorder channel, a queue a,b,c and `a` "audible".
    fn listen_engine() -> (Engine, tokio::sync::mpsc::UnboundedReceiver<WriterMsg>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let ids: Vec<String> = ["a", "b", "c"]
            .iter()
            .map(|n| {
                let p = dir.path().join(format!("{n}.wav"));
                write_min_wav(&p);
                p.to_string_lossy().into_owned()
            })
            .collect();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut engine = test_engine_with(Arc::new(crate::audio::source::LocalFileSource));
        engine.listen_tx = Some(tx);
        engine.handle(EngineCommand::PlayQueue { track_ids: ids, start_index: 0 });
        engine.status = PlaybackStatus::Playing; // no device in tests: pretend the probe finished
        engine.listen_sample();
        (engine, rx, dir)
    }

    fn name(id: &str) -> &str {
        std::path::Path::new(id).file_stem().and_then(|s| s.to_str()).unwrap()
    }

    fn session(rx: &mut tokio::sync::mpsc::UnboundedReceiver<WriterMsg>) -> Option<crate::listen::ListenSession> {
        match rx.try_recv().ok()? {
            WriterMsg::Session(s) => Some(s),
            WriterMsg::Flush(_) => None,
        }
    }

    #[test]
    fn engine_labels_next_jump_and_previous() {
        let (mut engine, mut rx, _dir) = listen_engine();
        assert!(rx.try_recv().is_err(), "nothing ended yet");
        engine.handle(EngineCommand::Next);
        engine.listen_sample();
        let s = session(&mut rx).unwrap();
        assert_eq!((name(&s.track_id), s.end_reason), ("a", EndReason::Next));
        engine.status = PlaybackStatus::Playing;
        engine.listen_sample(); // b is audible now
        engine.handle(EngineCommand::JumpTo(0));
        engine.listen_sample();
        let s = session(&mut rx).unwrap();
        assert_eq!((name(&s.track_id), s.end_reason), ("b", EndReason::Jump));
        engine.status = PlaybackStatus::Playing;
        engine.listen_sample();
        engine.handle(EngineCommand::Next);
        engine.status = PlaybackStatus::Playing;
        engine.listen_sample();
        engine.handle(EngineCommand::Previous);
        engine.listen_sample();
        let reasons: Vec<_> = std::iter::from_fn(|| session(&mut rx)).map(|s| (name(&s.track_id).to_string(), s.end_reason)).collect();
        assert_eq!(reasons, vec![("a".to_string(), EndReason::Next), ("b".to_string(), EndReason::Previous)]);
    }

    #[test]
    fn a_new_queue_ends_the_session_as_jump() {
        let (mut engine, mut rx, _dir) = listen_engine();
        engine.handle(EngineCommand::PlayQueue { track_ids: vec!["x".into()], start_index: 0 });
        engine.listen_sample();
        let s = session(&mut rx).unwrap();
        assert_eq!((name(&s.track_id), s.end_reason), ("a", EndReason::Jump));
    }

    #[test]
    fn quit_writes_straight_into_the_db_and_a_following_stop_adds_no_second_row() {
        let count = |e: &Engine| -> i64 { e.db.read(|c| c.query_row("SELECT COUNT(*) FROM listen_events", [], |r| r.get(0))).unwrap() };
        let (mut engine, mut rx, _dir) = listen_engine();
        let (tx, done) = std::sync::mpsc::channel();
        engine.handle(EngineCommand::PersistNow(tx));
        done.recv().unwrap();
        assert_eq!(count(&engine), 1);
        let reason: String = engine.db.read(|c| c.query_row("SELECT end_reason FROM listen_events", [], |r| r.get(0))).unwrap();
        assert_eq!(reason, "quit");
        // The update installer path: PersistNow, then Stop, while the track
        // is still playing in between (the run loop samples it again).
        engine.listen_sample();
        engine.handle(EngineCommand::Stop);
        assert!(rx.try_recv().is_err(), "no reopened session, no `stopped` duplicate");
        assert_eq!(count(&engine), 1);
    }

    #[test]
    fn stop_goes_through_the_writer_and_flush_follows_it() {
        let (mut engine, mut rx, _dir) = listen_engine();
        engine.handle(EngineCommand::Stop);
        let (tx, _done) = tokio::sync::oneshot::channel();
        engine.handle(EngineCommand::FlushListen(tx));
        match rx.try_recv() {
            Ok(WriterMsg::Session(s)) => assert_eq!((name(&s.track_id), s.end_reason), ("a", EndReason::Stopped)),
            other => panic!("expected the stopped session first, got {other:?}"),
        }
        assert!(matches!(rx.try_recv(), Ok(WriterMsg::Flush(_))), "flush queued behind it");
        let db_rows: i64 = engine.db.read(|c| c.query_row("SELECT COUNT(*) FROM listen_events", [], |r| r.get(0))).unwrap();
        assert_eq!(db_rows, 0, "Stop no longer writes on the engine thread");
    }

    #[test]
    fn flush_without_a_recorder_answers_at_once() {
        let mut engine = test_engine_with(Arc::new(crate::audio::source::LocalFileSource));
        let (tx, mut done) = tokio::sync::oneshot::channel();
        engine.handle(EngineCommand::FlushListen(tx));
        assert!(done.try_recv().is_ok());
    }
}
