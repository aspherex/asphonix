use super::*;

impl Engine {
    pub(super) fn pump(&mut self) {
        self.poll_pending_start();
        self.check_parked();
        self.pump_render_events();
        self.check_boundary();
        self.pump_decode_events();
    }

    /// A parked output only lives while its replacement track is loading,
    /// and only while its device is still there.
    pub(super) fn check_parked(&mut self) {
        let Some(pb) = &self.parked else { return };
        let lost = matches!(pb.render_events.try_recv(), Ok(RenderEvent::DeviceLost(_)));
        if lost || self.pending_start.is_none() {
            self.shutdown_parked();
        }
    }

    pub(super) fn pump_render_events(&mut self) {
        let Some(pb) = &self.playback else { return };
        let Ok(RenderEvent::DeviceLost(reason)) = pb.render_events.try_recv() else {
            return;
        };
        tracing::warn!(error = %reason, "output device lost");
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
    pub(super) fn pump_decode_events(&mut self) {
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
                Ok(DecodeEvent::Error(e)) => tracing::error!(track_id = %pb.current.id, error = %e, "decode error"),
                Ok(DecodeEvent::FormatReady(_)) => {}
                Err(_) => return,
            }
        }
    }

    /// The feeding decoder finished. Works out where the boundary is in the
    /// ring and lines up the next track behind it — the switch itself
    /// happens in `check_boundary` once the boundary has been played.
    pub(super) fn handle_track_eof(&mut self, producer: Producer<u8>, frames_pushed: u64) {
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
                        tracing::info!(
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
                    tracing::error!(error = %e, "could not open next track");
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
    pub(super) fn check_boundary(&mut self) {
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
                tracing::info!(
                    track_id = %next.id,
                    ring_frame = pending.boundary,
                    gap_frames = pb.render.gap_frames.load(Ordering::Relaxed),
                    stream_parks = pb.render.stream_parks.load(Ordering::Relaxed),
                    "gapless track change (gap_frames: underrun silence in this stream; stream_parks: long-pause stops)"
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
                tracing::info!(
                    track_id = %next.id,
                    format = %out.negotiated.format_label(""),
                    duration_ms = reopen_started.elapsed().as_millis() as u64,
                    "format change: output reopened"
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

}
