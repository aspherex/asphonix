use super::*;

impl Engine {
    pub(super) fn handle(&mut self, cmd: EngineCommand) {
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
                        tracing::warn!(error = %e.message, "flow: feature read failed, sequencing blind");
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
                tracing::info!("playback stopped");
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

}
