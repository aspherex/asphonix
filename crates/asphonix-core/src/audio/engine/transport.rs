use super::*;

impl Engine {
    pub(super) fn position_sec(&self) -> f64 {
        match &self.playback {
            Some(pb) => pb.position_sec(),
            None => self.resume_at.unwrap_or(0.0),
        }
    }

    pub(super) fn resume_or_start(&mut self) {
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
    pub(super) fn pause(&mut self) {
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

    pub(super) fn advance(&mut self, forward: bool) {
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
    pub(super) fn stop_at_end_of_queue(&mut self, forward: bool) {
        tracing::info!("playback: end of queue");
        if forward && self.prefs.autoplay() && self.autoplay.inflight.is_some() {
            self.autoplay.start_on_arrival = true;
            self.status = PlaybackStatus::Loading;
        } else {
            self.status = PlaybackStatus::Stopped;
        }
    }

    pub(super) fn seek(&mut self, seconds: f64) {
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
            super::super::ring::new_pcm_ring(pb.negotiated.sample_rate, pb.negotiated.bytes_per_frame());
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
    pub(super) fn loop_cycle(&mut self) {
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
    pub(super) fn loop_tick(&mut self) {
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
    pub(super) fn restart_output(&mut self) {
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
    pub(super) fn restart_current_reusing_downloads(&mut self, at: f64, paused: bool) {
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
    pub(super) fn reuse_download_for_current(&mut self) {
        let Some(id) = self.queue.current_id().map(str::to_string) else { return };
        if let Some(ms) = self.source.reattach(&id) {
            self.prefetched = Some((id, ms));
        }
    }

    pub(super) fn queue_edited(&mut self) {
        if let Some(Pending { queue_after, .. }) =
            self.playback.as_mut().and_then(|pb| pb.pending.as_mut())
        {
            *queue_after = None;
        }
    }

}
