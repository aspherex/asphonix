use super::*;

impl Engine {
    /// Spawns a decode thread for `id` and returns its command/event channels
    /// immediately — `TrackSource::open` only opens a local temp file or
    /// starts a background download, so this never blocks; the probe itself
    /// (which can block on a stalled HTTP connection) runs on the spawned
    /// thread and is polled, never awaited synchronously here.
    pub(super) fn spawn_probe(&mut self, id: &str) -> Result<(Sender<DecodeCommand>, Receiver<DecodeEvent>), String> {
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
    pub(super) fn open_decoder_blocking(&mut self, id: &str) -> Result<(Feeder, SourceFormat), String> {
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
    pub(super) fn open_output(
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
                super::super::ring::new_pcm_ring(negotiated.sample_rate, negotiated.bytes_per_frame());
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
                    tracing::warn!(error = %reason, "exclusive open failed; falling back to shared mode");
                    self.exclusive_error = Some(format!(
                        "Exclusive mode could not start ({reason}), so this track plays in shared mode."
                    ));
                    negotiated = output::shared_format(source);
                }
                Err(reason) => {
                    tracing::error!(error = %reason, "output open failed");
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
    pub(super) fn start_current(&mut self, at: Option<f64>) {
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

    pub(super) fn cancel_pending_start(&mut self) {
        if let Some(p) = self.pending_start.take() {
            p.stop();
        }
    }

    /// Polls the in-flight probe (if any) without blocking: moves to
    /// `begin_playback` once the format is ready, otherwise fails (and skips
    /// to the next queued track) on a decode error, a dead decoder thread, or
    /// `FORMAT_TIMEOUT` with no response at all (a hung/stalled connection).
    pub(super) fn poll_pending_start(&mut self) {
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
    pub(super) fn begin_playback(&mut self, p: PendingStart, source_format: SourceFormat) {
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
        if let Some(pb) = &self.playback {
            tracing::info!(
                track_id = %p.id,
                sample_rate = pb.negotiated.sample_rate,
                format = %pb.negotiated.format_label(""),
                mode = ?pb.negotiated.mode,
                paused = p.start_paused,
                "playback started"
            );
        }
        self.last_error = None; // a track is actually playing again
        self.consecutive_failures = 0;
        let queue = self.queue.clone();
        self.prefetch_after(&queue);
    }

    /// The parked output can carry `fmt` as it is: same user-requested mode
    /// (a shared fallback never silently stands in for a requested exclusive
    /// stream) and the gapless rule (`can_continue`) for rate/channels/container.
    pub(super) fn can_switch_in_place(&self, pb: &Playback, fmt: &SourceFormat) -> bool {
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
    pub(super) fn switch_in_place(
        &mut self,
        mut pb: Playback,
        feeder: Feeder,
        id: &str,
        fmt: SourceFormat,
        at: Option<f64>,
        start_paused: bool,
    ) -> Result<Playback, Feeder> {
        let (producer, consumer) =
            super::super::ring::new_pcm_ring(pb.negotiated.sample_rate, pb.negotiated.bytes_per_frame());
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
    pub(super) fn begin_decoder(feeder: &Feeder, container: Container, producer: Producer<u8>, at: Option<f64>) -> f64 {
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
    pub(super) fn open_playback(
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
    pub(super) fn fail_current_track(&mut self, reason: &str) {
        tracing::error!(error = %reason, "playback error");
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
    pub(super) fn current_default_id(&self) -> Option<String> {
        if self.output_device_id.is_some() {
            return None;
        }
        (self.default_id_lookup)()
    }

    pub(super) fn device_name(&self) -> Option<String> {
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

    pub(super) fn resolve_device_name(&self, endpoint: Option<&str>) -> Option<String> {
        let devices = output::list_devices();
        endpoint
            .and_then(|id| devices.iter().find(|d| d.id == id))
            .or_else(|| devices.iter().find(|d| d.is_default))
            .map(|d| d.name.clone())
    }

    /// Returns a prefetched `MediaSource` for `id` if one's already
    /// downloading, otherwise opens it fresh.
    pub(super) fn take_or_open(
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
    pub(super) fn prefetch_after(&mut self, queue: &Queue) {
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
            Err(e) => tracing::warn!(track_id = %next_id, error = %e, "prefetch failed"),
        }
    }

    pub(super) fn teardown_playback(&mut self) {
        self.shutdown_parked();
        if let Some(pb) = self.playback.take() {
            pb.shutdown();
        }
    }

    pub(super) fn shutdown_parked(&mut self) {
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
    pub(super) fn park_for_skip(&mut self) {
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
            super::super::ring::new_pcm_ring(pb.negotiated.sample_rate, pb.negotiated.bytes_per_frame());
        drop(producer);
        if lost || pb.render.send_ring(consumer).is_err() {
            pb.shutdown();
            return;
        }
        pb.seeks_sent += 1;
        self.parked = Some(pb);
    }

    // --- Event pump --------------------------------------------------------

}
