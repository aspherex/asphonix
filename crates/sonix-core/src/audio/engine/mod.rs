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


/// Same threshold the render thread uses to skip the gain stage entirely, so
/// "unity" here means "not a single sample is touched".
fn gain_is_unity(g: f32) -> bool {
    (g - 1.0).abs() <= f32::EPSILON
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
        tracing::warn!(error = %e, "spectrum thread not started");
    }
}


mod commands;
mod continuity;
mod lifecycle;
mod playback;
mod pump;
mod state;
mod transport;

#[cfg(test)]
mod tests;
