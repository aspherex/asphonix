//! Counting plays (local listening history + server scrobble) and "now playing" reports, driven by the engine's
//! in-process `watch<PlayerState>`. Vendored from the reference `scrobble/mod.rs`; `tauri::AppHandle`/`AppState`
//! replaced by [`ScrobbleCtx`] (the same shared `Arc`s). Gating is identical: history is always recorded,
//! every network call checks `scrobbling` first.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use super::is_new_play;
use super::queue::ScrobbleQueue;
use super::threshold::PlayThresholdTracker;
use crate::audio::Prefs;
use crate::ipc::{PlaybackStatus, PlayerState};
use crate::library::Db;
use crate::subsonic::{Client, PlaybackReportState};

/// How often the retry queue is drained (scrobbles aren't time-critical).
const RETRY_INTERVAL: Duration = Duration::from_secs(45);

/// Shared state the scrobble tasks need (what the reference reads from `AppState`).
#[derive(Clone)]
pub struct ScrobbleCtx {
    pub client: Arc<RwLock<Option<Client>>>,
    pub db: Arc<Db>,
    /// `Prefs::play_threshold_sec` is the live "count a play after" value.
    pub prefs: Arc<Prefs>,
    /// `Settings.scrobbling_enabled`, live.
    pub scrobbling: Arc<AtomicBool>,
    /// Directory of `scrobble_queue.sqlite3`.
    pub data_dir: std::path::PathBuf,
}

/// Starts the background tasks (play counter + now-playing service). No network I/O until a play counts / starts,
/// and none at all while `scrobbling` is false.
pub fn spawn(ctx: ScrobbleCtx, rx: tokio::sync::watch::Receiver<PlayerState>) {
    let queue = match ScrobbleQueue::open(&ctx.data_dir) {
        Ok(q) => Some(Arc::new(q)),
        Err(e) => {
            log::warn!("scrobble retry queue unavailable ({}); failed submissions will not be retried", e.message);
            None
        }
    };
    spawn_counter(ctx.clone(), queue.clone(), rx.clone());
    crate::rt::spawn(async move {
        let mut rx = rx;
        let mut svc = Service::new();
        let mut retry_tick = tokio::time::interval(RETRY_INTERVAL);
        retry_tick.tick().await;
        loop {
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let state = rx.borrow_and_update().clone();
                    svc.on_player_state(&ctx, &state).await;
                }
                _ = retry_tick.tick() => {
                    if let Some(q) = &queue {
                        drain_retry_queue(&ctx, q).await;
                    }
                }
            }
        }
    });
}

struct Play {
    track_id: String,
    last_position_sec: f64,
    last_status: PlaybackStatus,
    playback_report: Option<bool>,
}

/// The single "does this play count?" decision for both the local listening history and the server scrobble.
#[derive(Default)]
pub struct PlayCounter {
    current: Option<CountedPlay>,
}

struct CountedPlay {
    track_id: String,
    tracker: PlayThresholdTracker,
    started_at_ms: i64,
    last_position_sec: f64,
}

/// A play that just crossed the threshold.
#[derive(Debug, PartialEq)]
pub struct CountedHit {
    pub track_id: String,
    pub started_at_ms: i64,
}

impl PlayCounter {
    /// Feeds one player-state sample; returns the play exactly once, at the sample where it crosses the threshold.
    pub fn observe(&mut self, track_id: Option<&str>, playing: bool, position_sec: f64, duration_sec: f64, threshold_sec: u32, now_ms: i64) -> Option<CountedHit> {
        let Some(track_id) = track_id else {
            self.current = None;
            return None;
        };
        let is_new = match &self.current {
            None => true,
            Some(p) => is_new_play(&p.track_id, p.last_position_sec, duration_sec, track_id, position_sec),
        };
        if is_new {
            self.current = Some(CountedPlay { track_id: track_id.to_string(), tracker: PlayThresholdTracker::new(), started_at_ms: now_ms, last_position_sec: position_sec });
        }
        let p = self.current.as_mut()?;
        p.tracker.observe(playing, position_sec);
        p.last_position_sec = position_sec;
        if p.tracker.should_count(duration_sec, threshold_sec) {
            p.tracker.mark_scrobbled();
            return Some(CountedHit { track_id: p.track_id.clone(), started_at_ms: p.started_at_ms });
        }
        None
    }
}

fn spawn_counter(ctx: ScrobbleCtx, queue: Option<Arc<ScrobbleQueue>>, mut rx: tokio::sync::watch::Receiver<PlayerState>) {
    crate::rt::spawn(async move {
        let mut counter = PlayCounter::default();
        while rx.changed().await.is_ok() {
            let threshold_sec = ctx.prefs.play_threshold_sec();
            let hit = {
                let s = rx.borrow_and_update();
                counter.observe(s.track.as_ref().map(|t| t.id.as_str()), s.status == PlaybackStatus::Playing, s.position_sec, s.duration_sec, threshold_sec, chrono::Utc::now().timestamp_millis())
            };
            let Some(hit) = hit else { continue };
            let scrobbling = ctx.scrobbling.load(Ordering::Relaxed);
            let db = ctx.db.clone();
            let played_at = chrono::Utc::now().timestamp_millis();
            let history_id = hit.track_id.clone();
            crate::rt::spawn_blocking(move || {
                if let Err(e) = crate::library::discovery::history_record(&db, &history_id, played_at, scrobbling) {
                    log::warn!("failed to record listening history: {}", e.message);
                }
            });
            if scrobbling {
                let ctx = ctx.clone();
                let queue = queue.clone();
                crate::rt::spawn(async move {
                    let client = ctx.client.read().unwrap_or_else(|p| p.into_inner()).clone();
                    match client {
                        Some(client) => submit_classic_scrobble(&client, queue.as_deref(), &hit.track_id, hit.started_at_ms).await,
                        None => log::debug!("scrobble for {} dropped: no account connected", hit.track_id),
                    }
                    mirror_play_locally(&ctx, &hit.track_id, hit.started_at_ms);
                });
            }
        }
    });
}

struct Service {
    current: Option<Play>,
    playback_report_supported: Option<bool>,
}

impl Service {
    fn new() -> Self {
        Self { current: None, playback_report_supported: None }
    }

    /// Now-playing reports only; counting is [`PlayCounter`]'s job.
    async fn on_player_state(&mut self, ctx: &ScrobbleCtx, state: &PlayerState) {
        if !ctx.scrobbling.load(Ordering::Relaxed) {
            self.current = None;
            return;
        }
        let Some(track) = &state.track else {
            if let Some(play) = self.current.take() {
                Self::finalize(ctx, play).await;
            }
            return;
        };
        // A restored-but-never-played session is not a play.
        if self.current.is_none() && state.status != PlaybackStatus::Playing {
            return;
        }
        let is_new = match &self.current {
            None => true,
            Some(play) => is_new_play(&play.track_id, play.last_position_sec, state.duration_sec, &track.id, state.position_sec),
        };
        if is_new {
            if let Some(prev) = self.current.take() {
                Self::finalize(ctx, prev).await;
            }
            let client = ctx.client.read().unwrap_or_else(|p| p.into_inner()).clone();
            if self.playback_report_supported.is_none() {
                self.playback_report_supported = probe_playback_report(client.as_ref()).await;
            }
            let play = Play { track_id: track.id.clone(), last_position_sec: state.position_sec, last_status: state.status, playback_report: self.playback_report_supported };
            if let Some(client) = &client {
                if play.playback_report == Some(true) {
                    if let Err(e) = client.report_playback(&track.id, PlaybackReportState::Starting, 0, true).await {
                        log::debug!("reportPlayback(starting) failed: {}", e.message);
                    }
                } else if let Err(e) = client.scrobble(&track.id, false, None).await {
                    log::debug!("scrobble now-playing failed: {}", e.message);
                }
            }
            self.current = Some(play);
            return;
        }
        let Some(play) = self.current.as_mut() else { return };
        if play.playback_report == Some(true) && state.status != play.last_status {
            let report_state = match state.status {
                PlaybackStatus::Playing => Some(PlaybackReportState::Playing),
                PlaybackStatus::Paused => Some(PlaybackReportState::Paused),
                PlaybackStatus::Stopped | PlaybackStatus::Loading => None,
            };
            let client = ctx.client.read().unwrap_or_else(|p| p.into_inner()).clone();
            if let (Some(rs), Some(client)) = (report_state, client) {
                let position_ms = (state.position_sec.max(0.0) * 1000.0) as u64;
                if let Err(e) = client.report_playback(&track.id, rs, position_ms, true).await {
                    log::debug!("reportPlayback failed: {}", e.message);
                }
            }
        }
        play.last_position_sec = state.position_sec;
        play.last_status = state.status;
    }

    async fn finalize(ctx: &ScrobbleCtx, play: Play) {
        if play.playback_report != Some(true) {
            return;
        }
        let client = ctx.client.read().unwrap_or_else(|p| p.into_inner()).clone();
        let Some(client) = client else { return };
        let position_ms = (play.last_position_sec.max(0.0) * 1000.0) as u64;
        if let Err(e) = client.report_playback(&play.track_id, PlaybackReportState::Stopped, position_ms, true).await {
            log::debug!("reportPlayback(stopped) failed: {}", e.message);
        }
    }
}

/// Bumps the local `tracks.play_count`/`played` for a scrobbled play (smart-playlist rules), local SQLite only.
fn mirror_play_locally(ctx: &ScrobbleCtx, track_id: &str, started_at_ms: i64) {
    let played_at = chrono::DateTime::from_timestamp_millis(started_at_ms).unwrap_or_else(chrono::Utc::now).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    if let Err(e) = crate::library::record_local_play(&ctx.db, track_id, &played_at) {
        log::debug!("local play-stat update for {track_id} failed: {}", e.message);
    }
}

async fn probe_playback_report(client: Option<&Client>) -> Option<bool> {
    let client = client?;
    match client.get_open_subsonic_extensions().await {
        Ok(extensions) => Some(extensions.iter().any(|e| e == "playbackReport")),
        Err(e) => {
            log::debug!("getOpenSubsonicExtensions probe failed: {}", e.message);
            None
        }
    }
}

async fn submit_classic_scrobble(client: &Client, queue: Option<&ScrobbleQueue>, track_id: &str, started_at_ms: i64) {
    if let Err(e) = client.scrobble(track_id, true, Some(started_at_ms)).await {
        log::debug!("scrobble submission failed, queueing for retry: {}", e.message);
        queue_or_warn(queue, &account_of(client), track_id, started_at_ms);
    }
}

/// The account a client submits for (`"{username}@{url}"`): queued scrobbles are scoped to it.
pub fn account_of(client: &Client) -> String {
    crate::secrets::profiles::profile_account(client.base_url().as_str(), client.username())
}

fn queue_or_warn(queue: Option<&ScrobbleQueue>, account: &str, track_id: &str, started_at_ms: i64) {
    match queue {
        Some(q) => {
            if let Err(e) = q.enqueue_for(account, track_id, started_at_ms) {
                log::warn!("failed to queue scrobble retry for {track_id}: {}", e.message);
            }
        }
        None => log::warn!("scrobble for {track_id} lost: no retry queue available"),
    }
}

async fn drain_retry_queue(ctx: &ScrobbleCtx, queue: &ScrobbleQueue) {
    if !ctx.scrobbling.load(Ordering::Relaxed) {
        return;
    }
    let Some(client) = ctx.client.read().unwrap_or_else(|p| p.into_inner()).clone() else { return };
    drain_for(&client, queue).await;
}

/// Replays the rows queued for `client`'s account only; rows of other accounts are deleted, never submitted.
pub async fn drain_for(client: &Client, queue: &ScrobbleQueue) {
    let account = account_of(client);
    match queue.purge_other_accounts(&account) {
        Ok(0) => {}
        Ok(n) => log::info!("dropped {n} queued scrobble(s) of another account"),
        Err(e) => log::warn!("failed to purge scrobble retry queue: {}", e.message),
    }
    let pending = match queue.pending_for(&account) {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => return,
        Err(e) => {
            log::warn!("failed to read scrobble retry queue: {}", e.message);
            return;
        }
    };
    for row in pending {
        match client.scrobble(&row.track_id, true, Some(row.time_ms)).await {
            Ok(()) => {
                let _ = queue.remove(row.id);
            }
            Err(e) => {
                let _ = queue.record_failed_attempt(row.id, &e.message);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_change_is_a_new_play() {
        assert!(is_new_play("track-a", 50.0, 200.0, "track-b", 0.0));
    }

    #[test]
    fn repeat_one_restart_from_near_end_to_start_is_a_new_play() {
        // Track finished (position essentially at duration) and the same
        // id starts again from zero: the engine's repeat-one boundary.
        assert!(is_new_play("track-a", 199.6, 200.0, "track-a", 0.0));
    }

    #[test]
    fn ordinary_forward_playback_is_not_a_new_play() {
        assert!(!is_new_play("track-a", 50.0, 200.0, "track-a", 50.25));
    }

    #[test]
    fn seeking_backward_near_the_end_is_not_mistaken_for_a_restart() {
        // User drags the scrubber from 199s back to 190s: still near the
        // end, position decreased, but it never reached "near start".
        assert!(!is_new_play("track-a", 199.0, 200.0, "track-a", 190.0));
    }

    #[test]
    fn seeking_to_the_very_start_mid_track_is_not_mistaken_for_a_restart() {
        // User seeks back to 0 from the *middle* of the track (not near the
        // end yet) — not a repeat-one boundary, just a seek.
        assert!(!is_new_play("track-a", 90.0, 200.0, "track-a", 0.0));
    }

    #[test]
    fn jumping_forward_near_the_end_is_not_a_new_play() {
        assert!(!is_new_play("track-a", 50.0, 200.0, "track-a", 198.0));
    }

    /// Plays `id` from `from` to `to` in 250 ms engine ticks at a 30 s
    /// threshold; returns how many plays the counter reported.
    fn run(c: &mut PlayCounter, id: &str, from: f64, to: f64, dur: f64, playing: bool) -> usize {
        run_at(c, id, from, to, dur, playing, 30)
    }

    fn run_at(c: &mut PlayCounter, id: &str, from: f64, to: f64, dur: f64, playing: bool, threshold: u32) -> usize {
        let mut hits = 0;
        let mut p = from;
        while p <= to {
            hits += c.observe(Some(id), playing, p, dur, threshold, 0).is_some() as usize;
            p += 0.25;
        }
        hits
    }

    #[test]
    fn counts_exactly_once_per_play_across_pause_and_seek_back() {
        let mut c = PlayCounter::default();
        assert_eq!(run(&mut c, "a", 0.0, 20.0, 200.0, true), 0, "below 30 s");
        assert_eq!(run(&mut c, "a", 20.0, 20.0, 200.0, false), 0, "paused");
        assert_eq!(run(&mut c, "a", 20.25, 120.0, 200.0, true), 1, "crosses 30 s once");
        // Seek back to the start mid-track and listen past the threshold again.
        assert_eq!(run(&mut c, "a", 0.0, 150.0, 200.0, true), 0, "a seek is not a new play");
    }

    #[test]
    fn counts_repeat_one_loops_and_gapless_track_changes() {
        let mut c = PlayCounter::default();
        assert_eq!(run(&mut c, "a", 0.0, 199.75, 200.0, true), 1);
        // Repeat-one: same id restarts from zero.
        assert_eq!(run(&mut c, "a", 0.0, 199.75, 200.0, true), 1);
        // Gapless hand-off to the next track.
        assert_eq!(run(&mut c, "b", 0.0, 100.0, 180.0, true), 1);
        // Skipped before 30 s: not counted.
        assert_eq!(run(&mut c, "d", 0.0, 29.0, 300.0, true), 0);
        // Too-short tracks never count.
        assert_eq!(run(&mut c, "c", 0.0, 25.0, 25.0, true), 0);
        // Stop clears the play; the same track started again is a new play.
        assert!(c.observe(None, false, 0.0, 0.0, 30, 0).is_none());
        assert_eq!(run(&mut c, "b", 0.0, 100.0, 180.0, true), 1);
    }

    #[test]
    fn follows_the_configured_threshold() {
        let mut c = PlayCounter::default();
        assert_eq!(run_at(&mut c, "a", 0.0, 50.0, 300.0, true, 60), 0, "60 s setting, 50 s heard");
        assert_eq!(run_at(&mut c, "a", 50.25, 61.0, 300.0, true, 60), 1);
    }

    #[test]
    fn a_hit_carries_the_play_start_time_for_the_scrobble() {
        let mut c = PlayCounter::default();
        assert!(c.observe(Some("a"), true, 0.0, 200.0, 30, 1_000).is_none());
        let mut hit = None;
        let mut p = 0.25;
        while hit.is_none() && p <= 40.0 {
            hit = c.observe(Some("a"), true, p, 200.0, 30, 99_000);
            p += 0.25;
        }
        assert_eq!(hit, Some(CountedHit { track_id: "a".into(), started_at_ms: 1_000 }));
    }
}
