//! Continuity: session persistence, restore, autoplay.

use super::*;

impl Engine {
    /// Called once per run-loop iteration, after `pump()`. Everything here is
    /// O(1) unless something actually changed.
    pub(super) fn continuity_tick(&mut self) {
        self.loop_tick();
        self.observe_audible();
        self.poll_autoplay();
        self.maybe_request_autoplay();
        self.persist_tick();
    }

    /// Remembers what was actually audible (not merely queued/restored) for
    /// autoplay's "don't repeat the last ~50 tracks" rule.
    pub(super) fn observe_audible(&mut self) {
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

    pub(super) fn reset_autoplay(&mut self) {
        self.autoplay = AutoplayState::default();
    }

    /// Starts the similar-tracks lookup the moment the *last* queued track is
    /// playing, so the answer is normally in before it ends (gapless).
    pub(super) fn maybe_request_autoplay(&mut self) {
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
        let spawned = std::thread::Builder::new().name("asphonix-autoplay".into()).spawn(move || {
            let _ = tx.send(provider.similar(&lookup_id));
        });
        match spawned {
            Ok(_) => self.autoplay.inflight = Some(InflightAutoplay { seed, rx }),
            Err(e) => tracing::warn!(error = %e, "autoplay lookup thread not started"),
        }
    }

    pub(super) fn poll_autoplay(&mut self) {
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
    pub(super) fn apply_autoplay(&mut self, seed: &str, items: Vec<(String, Option<String>)>) {
        // Only still "waiting" if nothing was started since the queue ran out
        // (a Previous/JumpTo/new queue in the meantime owns the status now).
        let waiting = std::mem::take(&mut self.autoplay.start_on_arrival)
            && self.queue.current_id().is_none()
            && self.playback.is_none()
            && self.pending_start.is_none();
        let queued: std::collections::HashSet<String> = self.queue.ids().map(str::to_string).collect();
        let (ids, artists) = autoplay::filter_candidates(items, &self.recent_played, &|id| queued.contains(id));
        if !self.prefs.autoplay() || ids.is_empty() {
            tracing::info!(track_id = %seed, "autoplay: nothing to continue with");
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
            tracing::info!(track_id = %seed, "autoplay: queued similar tracks");
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

    pub(super) fn session_request(&mut self, force_full: bool, push: PushKind) -> SaveRequest {
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
    pub(super) fn persist_tick(&mut self) {
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
    pub(super) fn persist_final(&mut self) {
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
    pub(super) fn restore_session(&mut self, state: SessionState, pools: Option<SourcePools>, force: bool) {
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
                tracing::warn!(error = %e.message, "session restore: library read failed");
                return;
            }
        };
        if known.is_empty() {
            tracing::info!("session restore skipped: none of the saved tracks is in the local library");
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
                Err(e) => tracing::warn!(error = %e.message, "session restore: flow data read failed"),
            }
        }
        if restored.dropped > 0 {
            tracing::info!(count = restored.dropped, "session restore: dropped unknown tracks");
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
