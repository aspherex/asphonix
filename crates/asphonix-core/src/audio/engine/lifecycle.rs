use super::*;

impl Engine {
    pub(super) fn new(
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

    pub(super) fn run(mut self, rx: Receiver<EngineCommand>) {
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
                    if tracing::enabled!(tracing::Level::INFO)
                        && let Some(what) = describe_queue_command(&cmd) {
                            tracing::info!("engine: {what}");
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
    pub(super) fn is_idle(&self) -> bool {
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
    pub(super) fn listen_sample(&mut self) {
        let state = self.player_state();
        self.listen_observe(&state);
    }

    pub(super) fn listen_observe(&mut self, state: &PlayerState) {
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
    pub(super) fn listen_close(&mut self, reason: EndReason, sync: bool) {
        let Some(tx) = &self.listen_tx else { return };
        let tx = tx.clone();
        self.listen_sample();
        let thr = self.prefs.play_threshold_sec();
        let Some(s) = self.listen.finish(reason, chrono::Utc::now().timestamp_millis(), thr) else { return };
        if !sync {
            let _ = tx.send(WriterMsg::Session(s));
        } else if let Err(e) = crate::listen::insert_session(&self.db, &s) {
            tracing::warn!(error = %e.message, "failed to record listen event");
        }
    }

}
