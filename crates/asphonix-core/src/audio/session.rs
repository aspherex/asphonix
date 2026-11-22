//! Playback session persistence (resume on restart) and Subsonic play-queue
//! sync (cross-device resume).
//!
//! ## Storage: two small JSON files next to `settings.json`
//!
//! A table in the library SQLite would have been wiped by the library's
//! drop-and-resync schema migrations (`library::schema`), taking the user's
//! session with it; plain files survive those and stay independent of the DB
//! handle (the engine already shares the DB with sync/browse readers).
//!
//! - `session.json`: queue core (the materialized window + history, bounded by
//!   `Queue::prune_history`, a few dozen ids), current index, position, volume,
//!   shuffle/repeat. Rewritten often.
//! - `session_source.json`: the rolling `Source`'s full pool and the current
//!   round's `remaining` tail (up to ~2x50k ids). Only rewritten when the
//!   source's [`SourceStamp`] changes (new source, a window refill, a new
//!   round), never for position ticks. Restoring it is what makes a 1,980-track
//!   shuffle come back with the *same* remaining round instead of a new one.
//!   The stamp is stored in both files; a mismatch (crash between the two
//!   writes) drops the source on restore rather than mixing rounds.
//!
//! ## Threads
//!
//! The engine thread never does disk or network I/O for this: it hands a
//! [`SaveRequest`] to the `asphonix-session` writer thread (a channel send). The
//! writer does the atomic file writes and forwards server pushes to a second
//! thread, `asphonix-session-push`, so a hung network can never delay a local
//! write (or app exit): it owns the throttling (>= 30 s between pushes, 60 s
//! for position-only updates) and the `savePlayQueue` call itself.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::prefs::Prefs;
use super::queue::{QueueCore, SourcePools};
use crate::subsonic::Client;

pub const SESSION_FILE: &str = "session.json";
pub const SESSION_SOURCE_FILE: &str = "session_source.json";
pub const SESSION_VERSION: u32 = 1;

/// The server's play queue has no notion of our rolling source, so we send
/// the materialized queue only, capped (history a few tracks behind current).
pub const MAX_SERVER_QUEUE: usize = 100;
const SERVER_QUEUE_HISTORY: usize = 10;

/// Minimum spacing between `savePlayQueue` calls (event-driven pushes).
const MIN_PUSH_INTERVAL: Duration = Duration::from_secs(30);
/// Position-only refreshes while playing are sent at most this often.
const PERIODIC_PUSH_INTERVAL: Duration = Duration::from_secs(60);
const PUSH_TIMEOUT: Duration = Duration::from_secs(8);
/// "Resume from server" is only offered when the server's queue is newer than
/// our last local activity by more than this (server/PC clocks may differ).
pub const CLOCK_SKEW_TOLERANCE_MS: i64 = 5_000;

/// What `session.json` holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub version: u32,
    /// Unix ms of the last write (local clock) = our last local activity.
    pub saved_at_ms: i64,
    pub position_ms: u64,
    /// Volume slider position; `None` for a state built from the server queue.
    #[serde(default)]
    pub volume: Option<f32>,
    pub queue: QueueCore,
}

impl SessionState {
    pub fn current_id(&self) -> Option<&str> {
        self.queue.current.and_then(|i| self.queue.entries.get(i)).map(|e| e.id.as_str())
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn session_path(dir: &Path) -> PathBuf {
    dir.join(SESSION_FILE)
}

fn source_path(dir: &Path) -> PathBuf {
    dir.join(SESSION_SOURCE_FILE)
}

/// tmp + rename so a crash mid-write never leaves a half-written file.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// Reads the saved session (and the source pools, if present). Pure local
/// file I/O; a missing/corrupt/unknown-version file is simply "no session".
pub fn load(dir: &Path) -> Option<(SessionState, Option<SourcePools>)> {
    let state = load_state(dir)?;
    let pools = if state.queue.source.is_some() {
        fs::read(source_path(dir)).ok().and_then(|b| serde_json::from_slice::<SourcePools>(&b).ok())
    } else {
        None
    };
    Some((state, pools))
}

/// Just `session.json` (small), without the potentially MB-sized pools file.
fn load_state(dir: &Path) -> Option<SessionState> {
    let state: SessionState = serde_json::from_slice(&fs::read(session_path(dir)).ok()?).ok()?;
    (state.version == SESSION_VERSION).then_some(state)
}

pub fn clear_files(dir: &Path) {
    let _ = fs::remove_file(session_path(dir));
    let _ = fs::remove_file(source_path(dir));
}

fn write_files(dir: &Path, state: &SessionState, pools: Option<&SourcePools>) {
    if let Some(p) = pools {
        match serde_json::to_vec(p) {
            Ok(b) => {
                if let Err(e) = write_atomic(&source_path(dir), &b) {
                    log::warn!("session: writing source pools failed: {e}");
                }
            }
            Err(e) => log::warn!("session: serializing source pools failed: {e}"),
        }
    } else if state.queue.source.is_none() {
        let _ = fs::remove_file(source_path(dir)); // stale pools of an earlier source
    }
    match serde_json::to_vec(state) {
        Ok(b) => {
            if let Err(e) = write_atomic(&session_path(dir), &b) {
                log::warn!("session: writing session failed: {e}");
            }
        }
        Err(e) => log::warn!("session: serializing session failed: {e}"),
    }
}

/// The queue window sent to the server: up to `MAX_SERVER_QUEUE` entries
/// starting a few tracks before the current one, plus the current id.
pub fn server_queue_window(core: &QueueCore) -> (Vec<String>, Option<String>) {
    let Some(cur) = core.current.filter(|&c| c < core.entries.len()) else {
        return (Vec::new(), None);
    };
    let start = cur.saturating_sub(SERVER_QUEUE_HISTORY);
    let end = (start + MAX_SERVER_QUEUE).min(core.entries.len());
    let ids = core.entries[start..end].iter().map(|e| e.id.clone()).collect();
    (ids, Some(core.entries[cur].id.clone()))
}

/// Local-activity marker for the "Resume from server" decision.
#[derive(Debug, Clone, Default)]
pub struct LocalMark {
    pub saved_at_ms: i64,
    pub current_id: Option<String>,
}

/// Should the server's queue be offered as "Resume from server"? Only when it
/// names a different current track than ours *and* was changed after our last
/// local activity (so a stale server queue never overrides a fresher local
/// session). No local session at all counts as older than anything.
pub fn should_offer_server_queue(local: &LocalMark, server_current: Option<&str>, server_changed_ms: Option<i64>) -> bool {
    let Some(sc) = server_current else { return false };
    if local.current_id.as_deref() == Some(sc) {
        return false;
    }
    if local.saved_at_ms == 0 {
        return true;
    }
    match server_changed_ms {
        Some(s) => s > local.saved_at_ms + CLOCK_SKEW_TOLERANCE_MS,
        None => false,
    }
}

/// Parses a Subsonic `changed` timestamp (ISO 8601 / RFC 3339) to unix ms.
/// A timestamp without a zone (some servers) is read as UTC.
pub fn parse_server_time_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis())
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").map(|d| d.and_utc().timestamp_millis()))
        .ok()
}

// ---------------------------------------------------------------------------
// Writer + push threads
// ---------------------------------------------------------------------------

/// How eagerly a save should reach the server. Ordered: a pending job keeps
/// the strongest kind it has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PushKind {
    /// Local file only (a position tick while playing is `Periodic`).
    None,
    /// Position refresh while playing: sent at most every 60 s.
    Periodic,
    /// Track change / pause / queue edit: sent at most every 30 s.
    Event,
    /// App exit: sent immediately.
    Final,
}

impl PushKind {
    /// Minimum time since the previous push before this kind may be sent.
    fn min_interval(self) -> Duration {
        match self {
            PushKind::None | PushKind::Final => Duration::ZERO,
            PushKind::Periodic => PERIODIC_PUSH_INTERVAL,
            PushKind::Event => MIN_PUSH_INTERVAL,
        }
    }
}

/// How long until a job of `kind` may be sent, given the time since the last push.
fn push_wait(kind: PushKind, since_last: Option<Duration>) -> Duration {
    match since_last {
        None => Duration::ZERO,
        Some(d) => kind.min_interval().saturating_sub(d),
    }
}

pub struct SaveRequest {
    /// `Some` when the queue changed since the last save (the writer keeps the
    /// previous core and only merges the position otherwise).
    pub queue: Option<QueueCore>,
    /// `Some` only when the source's stamp changed.
    pub pools: Option<SourcePools>,
    pub position_ms: u64,
    pub volume: f32,
    pub push: PushKind,
}

enum Msg {
    Save(Box<SaveRequest>, Option<Sender<()>>),
    Clear,
}

struct PushJob {
    ids: Vec<String>,
    current: Option<String>,
    position_ms: u64,
    kind: PushKind,
    ack: Option<Sender<()>>,
}

pub struct SessionStore {
    tx: Mutex<Sender<Msg>>,
    mark: Arc<Mutex<LocalMark>>,
}

impl SessionStore {
    /// Starts the writer and push threads (no I/O until the writer thread
    /// reads the existing session's mark, off the caller's thread).
    pub fn spawn(dir: PathBuf, client: Arc<RwLock<Option<Client>>>, prefs: Arc<Prefs>) -> Arc<Self> {
        let (tx, rx) = channel::<Msg>();
        let (push_tx, push_rx) = channel::<PushJob>();
        let mark = Arc::new(Mutex::new(LocalMark::default()));
        let writer_mark = mark.clone();
        let writer_prefs = prefs.clone();
        let spawned = std::thread::Builder::new().name("asphonix-session".into()).spawn(move || {
            writer_loop(rx, dir, writer_mark, push_tx, writer_prefs);
        });
        if let Err(e) = spawned {
            log::warn!("session writer thread not started: {e}");
        }
        let spawned = std::thread::Builder::new().name("asphonix-session-push".into()).spawn(move || {
            push_loop(push_rx, client, prefs);
        });
        if let Err(e) = spawned {
            log::warn!("session push thread not started: {e}");
        }
        Arc::new(Self { tx: Mutex::new(tx), mark })
    }

    fn send(&self, msg: Msg) {
        let tx = self.tx.lock().unwrap_or_else(|p| p.into_inner());
        let _ = tx.send(msg);
    }

    /// Fire-and-forget; never blocks on disk or network.
    pub fn save(&self, req: SaveRequest) {
        self.send(Msg::Save(Box::new(req), None));
    }

    /// Exit path: saves and waits (bounded) until the files are written and
    /// the final server push, if enabled, completed.
    pub fn save_final(&self, mut req: SaveRequest, timeout: Duration) -> bool {
        req.push = PushKind::Final;
        let (ack_tx, ack_rx) = channel();
        self.send(Msg::Save(Box::new(req), Some(ack_tx)));
        ack_rx.recv_timeout(timeout).is_ok()
    }

    /// Logout / account switch: the saved ids belong to the old account.
    pub fn clear(&self) {
        self.send(Msg::Clear);
    }

    pub fn local_mark(&self) -> LocalMark {
        self.mark.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Records a state restored from the server queue / disk as our local
    /// activity marker without waiting for the next save.
    pub fn set_mark(&self, mark: LocalMark) {
        *self.mark.lock().unwrap_or_else(|p| p.into_inner()) = mark;
    }
}

fn writer_loop(rx: Receiver<Msg>, dir: PathBuf, mark: Arc<Mutex<LocalMark>>, push_tx: Sender<PushJob>, prefs: Arc<Prefs>) {
    // Only the small core file: the mark doesn't need the source pools.
    if let Some(state) = load_state(&dir) {
        *mark.lock().unwrap_or_else(|p| p.into_inner()) =
            LocalMark { saved_at_ms: state.saved_at_ms, current_id: state.current_id().map(str::to_string) };
    }
    let mut last_core: Option<QueueCore> = None;
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Clear => {
                clear_files(&dir);
                last_core = None;
                *mark.lock().unwrap_or_else(|p| p.into_inner()) = LocalMark::default();
            }
            Msg::Save(req, ack) => {
                if let Some(core) = req.queue {
                    last_core = Some(core);
                }
                let Some(core) = last_core.clone() else {
                    if let Some(a) = ack {
                        let _ = a.send(());
                    }
                    continue;
                };
                let state = SessionState {
                    version: SESSION_VERSION,
                    saved_at_ms: now_ms(),
                    position_ms: req.position_ms,
                    volume: Some(req.volume),
                    queue: core,
                };
                write_files(&dir, &state, req.pools.as_ref());
                *mark.lock().unwrap_or_else(|p| p.into_inner()) =
                    LocalMark { saved_at_ms: state.saved_at_ms, current_id: state.current_id().map(str::to_string) };
                let (ids, current) = server_queue_window(&state.queue);
                if req.push != PushKind::None && prefs.sync_play_queue() && !ids.is_empty() {
                    let _ = push_tx.send(PushJob { ids, current, position_ms: req.position_ms, kind: req.push, ack });
                } else if let Some(a) = ack {
                    let _ = a.send(());
                }
            }
        }
    }
}

fn push_loop(rx: Receiver<PushJob>, client: Arc<RwLock<Option<Client>>>, prefs: Arc<Prefs>) {
    let mut runtime: Option<tokio::runtime::Runtime> = None;
    let mut last_push: Option<Instant> = None;
    let mut last_sent: Option<(Vec<String>, Option<String>, u64)> = None;
    let mut pending: Option<PushJob> = None;
    loop {
        let wait = match &pending {
            Some(job) => push_wait(job.kind, last_push.map(|t| t.elapsed())),
            None => Duration::from_secs(3600),
        };
        match rx.recv_timeout(wait) {
            Ok(mut job) => {
                if let Some(old) = pending.take() {
                    job.kind = job.kind.max(old.kind);
                    // A superseded job's waiter (exit) is answered by the newer one.
                    if job.ack.is_none() {
                        job.ack = old.ack;
                    }
                }
                pending = Some(job);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let due = pending.as_ref().is_some_and(|j| push_wait(j.kind, last_push.map(|t| t.elapsed())).is_zero());
        if !due {
            continue;
        }
        let job = pending.take().expect("checked above");
        let same_as_last = last_sent.as_ref().is_some_and(|(ids, cur, pos)| {
            *ids == job.ids && *cur == job.current && pos.abs_diff(job.position_ms) < 3_000
        });
        if !prefs.sync_play_queue() || same_as_last {
            if let Some(a) = job.ack {
                let _ = a.send(());
            }
            continue;
        }
        let Some(c) = client.read().unwrap_or_else(|p| p.into_inner()).clone() else {
            if let Some(a) = job.ack {
                let _ = a.send(());
            }
            continue;
        };
        if runtime.is_none() {
            runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok();
        }
        let Some(rt) = &runtime else {
            if let Some(a) = job.ack {
                let _ = a.send(());
            }
            continue;
        };
        last_push = Some(Instant::now());
        let result = rt.block_on(async {
            tokio::time::timeout(PUSH_TIMEOUT, c.save_play_queue(&job.ids, job.current.as_deref(), job.position_ms)).await
        });
        match result {
            Ok(Ok(())) => last_sent = Some((job.ids.clone(), job.current.clone(), job.position_ms)),
            Ok(Err(e)) => log::debug!("savePlayQueue failed: {}", e.message),
            Err(_) => log::debug!("savePlayQueue timed out"),
        }
        if let Some(a) = job.ack {
            let _ = a.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::queue::{EntrySnapshot, Queue};
    use crate::ipc::{QueueItemOrigin, RepeatMode, SourceKind, SourceMode};

    fn core_of(n: usize, current: usize) -> QueueCore {
        QueueCore {
            entries: (0..n).map(|i| EntrySnapshot { id: format!("t{i}"), origin: QueueItemOrigin::Source }).collect(),
            current: Some(current),
            shuffle: false,
            repeat: RepeatMode::Off,
            static_natural_order: Vec::new(),
            source: None,
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("asphonix-session-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn session_state_roundtrips_through_disk_with_source_pools() {
        let dir = temp_dir("roundtrip");
        let mut q = Queue::new();
        q.set_window_size(10);
        q.set_source(SourceKind::Playlist, "Liked".into(), SourceMode::Shuffle, (0..200).map(|i| format!("t{i}")).collect(), vec![], None);
        q.next();
        let state = SessionState {
            version: SESSION_VERSION,
            saved_at_ms: 1_700_000_000_000,
            position_ms: 93_500,
            volume: Some(0.75),
            queue: q.snapshot_core(),
        };
        write_files(&dir, &state, q.snapshot_pools().as_ref());
        let (loaded, pools) = load(&dir).expect("session loads");
        assert_eq!(loaded, state);
        assert_eq!(loaded.position_ms, 93_500);
        let pools = pools.expect("pools loaded");
        assert_eq!(pools.pool.len(), 200);
        let restored = Queue::restore(loaded.queue, Some(pools), &|_| true, 10, false);
        assert_eq!(restored.queue.ordered_ids(), q.ordered_ids());
        assert_eq!(restored.queue.source_info(), q.source_info());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_from_disk_drops_deleted_ids_honestly() {
        let dir = temp_dir("deleted");
        let state = SessionState { version: SESSION_VERSION, saved_at_ms: 1, position_ms: 5_000, volume: None, queue: core_of(5, 2) };
        write_files(&dir, &state, None);
        let (loaded, pools) = load(&dir).unwrap();
        assert!(pools.is_none());
        // t2 (the current track) and t4 were deleted from the library.
        let r = Queue::restore(loaded.queue, None, &|id| id != "t2" && id != "t4", 20, false);
        assert_eq!(r.dropped, 2);
        assert!(!r.current_kept, "the saved position must not be applied to a different track");
        assert_eq!(r.queue.current_id(), Some("t3"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_corrupt_or_foreign_version_files_mean_no_session() {
        let dir = temp_dir("corrupt");
        assert!(load(&dir).is_none());
        fs::write(session_path(&dir), b"{ not json").unwrap();
        assert!(load(&dir).is_none());
        let mut state = SessionState { version: 99, saved_at_ms: 1, position_ms: 0, volume: None, queue: core_of(2, 0) };
        fs::write(session_path(&dir), serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(load(&dir).is_none(), "unknown version is ignored, not mis-parsed");
        state.version = SESSION_VERSION;
        fs::write(session_path(&dir), serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(load(&dir).is_some());
        clear_files(&dir);
        assert!(load(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_queue_is_capped_around_the_current_track() {
        let (ids, cur) = server_queue_window(&core_of(500, 250));
        assert_eq!(ids.len(), MAX_SERVER_QUEUE);
        assert_eq!(cur.as_deref(), Some("t250"));
        assert_eq!(ids[0], "t240", "a few tracks of history before the current one");
        assert!(ids.contains(&"t250".to_string()));
        let (ids, cur) = server_queue_window(&core_of(3, 0));
        assert_eq!(ids.len(), 3);
        assert_eq!(cur.as_deref(), Some("t0"));
        let mut none = core_of(3, 0);
        none.current = None;
        assert!(server_queue_window(&none).0.is_empty());
    }

    #[test]
    fn resume_offer_needs_a_different_track_and_a_newer_server_queue() {
        let local = LocalMark { saved_at_ms: 1_000_000, current_id: Some("a".into()) };
        // Same track: nothing to resume.
        assert!(!should_offer_server_queue(&local, Some("a"), Some(9_000_000)));
        // Different track, newer: offer.
        assert!(should_offer_server_queue(&local, Some("b"), Some(1_000_000 + CLOCK_SKEW_TOLERANCE_MS + 1)));
        // Different track but the server queue is older than our activity (or within clock skew).
        assert!(!should_offer_server_queue(&local, Some("b"), Some(500_000)));
        assert!(!should_offer_server_queue(&local, Some("b"), Some(1_000_000 + 1_000)));
        // Unknown server time: don't guess.
        assert!(!should_offer_server_queue(&local, Some("b"), None));
        // No server queue.
        assert!(!should_offer_server_queue(&local, None, Some(9_000_000)));
        // No local session at all: any server queue is worth offering.
        assert!(should_offer_server_queue(&LocalMark::default(), Some("b"), None));
    }

    #[test]
    fn parses_server_timestamps() {
        assert_eq!(parse_server_time_ms("2026-10-11T09:30:00Z"), Some(1_791_711_000_000));
        assert!(parse_server_time_ms("2026-10-11T09:30:00.123456789Z").is_some());
        assert_eq!(parse_server_time_ms("2026-10-11T09:30:00"), Some(1_791_711_000_000), "zone-less = UTC");
        assert_eq!(parse_server_time_ms("yesterday"), None);
    }

    #[test]
    fn push_throttling_respects_the_30s_and_60s_floors() {
        let s = |n| Some(Duration::from_secs(n));
        assert_eq!(push_wait(PushKind::Event, None), Duration::ZERO);
        assert_eq!(push_wait(PushKind::Event, s(10)), Duration::from_secs(20));
        assert_eq!(push_wait(PushKind::Event, s(45)), Duration::ZERO);
        assert_eq!(push_wait(PushKind::Periodic, s(45)), Duration::from_secs(15));
        assert_eq!(push_wait(PushKind::Final, s(0)), Duration::ZERO, "exit always pushes immediately");
        assert!(PushKind::Final > PushKind::Event && PushKind::Event > PushKind::Periodic);
    }

    #[test]
    fn writer_merges_position_only_saves_into_the_last_core() {
        let dir = temp_dir("writer");
        let store = SessionStore::spawn(dir.clone(), Arc::new(RwLock::new(None)), Arc::new(Prefs::default()));
        store.save(SaveRequest { queue: Some(core_of(4, 1)), pools: None, position_ms: 1_000, volume: 1.0, push: PushKind::Event });
        // Position-only tick: no queue attached.
        assert!(store.save_final(
            SaveRequest { queue: None, pools: None, position_ms: 42_000, volume: 0.5, push: PushKind::Periodic },
            Duration::from_secs(5)
        ));
        let (state, _) = load(&dir).expect("written");
        assert_eq!(state.position_ms, 42_000);
        assert_eq!(state.volume, Some(0.5));
        assert_eq!(state.queue.entries.len(), 4, "the earlier core was kept");
        assert_eq!(store.local_mark().current_id.as_deref(), Some("t1"));
        store.clear();
        // Clear is processed in order; a final ack on a following save proves it ran.
        assert!(store.save_final(
            SaveRequest { queue: None, pools: None, position_ms: 0, volume: 1.0, push: PushKind::None },
            Duration::from_secs(5)
        ));
        assert!(load(&dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
