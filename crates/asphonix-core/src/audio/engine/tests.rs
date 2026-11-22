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
        stream_parks: Arc::new(AtomicU64::new(0)),
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
    let dir = std::env::temp_dir().join(format!("asphonix-engine-session-{}", std::process::id()));
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
    let dir = std::env::temp_dir().join(format!("asphonix-engine-session-stop-{}", std::process::id()));
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
        stream_parks: Arc::new(AtomicU64::new(0)),
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
