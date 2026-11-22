use super::*;

impl Engine {
    /// Engine-authoritative: exclusive mode (native rate, bit-identical
    /// container) and a combined render gain (volume taper x ReplayGain) of
    /// exactly unity. See `PlayerState.bit_perfect`/`OutputStatus.bit_perfect`.
    pub(super) fn is_bit_perfect(&self) -> bool {
        self.playback
            .as_ref()
            .is_some_and(|pb| pb.negotiated.is_bit_perfect_capable())
            && gain_is_unity(self.effective_gain_for(self.audible_id()))
    }

    // --- ReplayGain ----------------------------------------------------

    /// `(linear, dB)` ReplayGain for `id` under the current mode, `None` =
    /// unity (mode off, or the track has no usable tag in the local library).
    pub(super) fn replay_gain_for(&self, id: &str) -> Option<(f32, f32)> {
        let mode = self.prefs.replay_gain();
        if mode == ReplayGainMode::Off {
            return None;
        }
        replaygain::replay_gain_factor(mode, self.track_cache.get(id).and_then(|t| t.replay_gain.as_ref()))
    }

    /// The one gain the render thread applies: the user's volume taper times
    /// the audible track's ReplayGain factor. Exactly 1.0 (no sample touched,
    /// bit-perfect) only when both are unity.
    pub(super) fn effective_gain_for(&self, audible_id: Option<&str>) -> f32 {
        let rg = audible_id.and_then(|id| self.replay_gain_for(id)).map_or(1.0, |(lin, _)| lin);
        output::volume_to_gain(self.volume) * rg
    }

    pub(super) fn audible_id(&self) -> Option<&str> {
        self.playback.as_ref().map(|pb| pb.current.id.as_str())
    }

    /// Pushes the combined gain for the *audible* track to the render thread.
    /// Called on volume changes, ReplayGain mode changes and when a track
    /// becomes audible. Gapless boundaries are committed by `check_boundary`
    /// (polled every `BOUNDARY_TICK` = 10 ms while a boundary is pending), so
    /// the new track's gain takes effect within ~10 ms + one render period of
    /// its first sample — the granularity is documented, not frame-exact.
    pub(super) fn apply_gain(&self) {
        if let Some(pb) = &self.playback {
            pb.render.set_volume(self.effective_gain_for(Some(&pb.current.id)));
        }
    }

    /// The ReplayGain dB currently applied to the audible track (after the
    /// peak clamp), only when it actually changes the signal.
    pub(super) fn applied_replay_gain_db(&self) -> Option<f32> {
        let (_, db) = self.audible_id().and_then(|id| self.replay_gain_for(id))?;
        (db.abs() >= 0.05).then_some(db)
    }

    /// Just the codec (e.g. `FLAC`): `PlayerState`/`OutputStatus` carry the
    /// mode, bit depth and rate as separate fields and the UI composes the
    /// readout from them, so repeating them here doubled up as
    /// "SHARED · 32/44 · SHARED · 32/44100 · FLAC".
    pub(super) fn format_label(&self) -> Option<String> {
        self.playback.as_ref().map(|pb| pb.current.format.codec_label())
    }

    pub(super) fn output_status(&self) -> OutputStatus {
        match &self.playback {
            Some(pb) => OutputStatus {
                device_id: self.output_device_id.clone(),
                device_name: pb.device_name.clone(),
                mode: pb.negotiated.mode,
                sample_rate: Some(pb.negotiated.sample_rate),
                bit_depth: Some(pb.negotiated.container.valid_bits()),
                format_label: self.format_label(),
                bit_perfect: self.is_bit_perfect(),
                exclusive_error: self.exclusive_error.clone(),
            },
            None => OutputStatus {
                device_id: self.output_device_id.clone(),
                device_name: None,
                mode: self.output_mode_pref,
                sample_rate: None,
                bit_depth: None,
                format_label: None,
                bit_perfect: false,
                exclusive_error: self.exclusive_error.clone(),
            },
        }
    }

    /// Resolves any of `ids` not already in `track_cache` from the local
    /// library DB (one query, batched), merging the results in. Called only
    /// from queue-mutating commands (`PlayQueue`/`Enqueue`) — never from the
    /// player-state/render-tick hot path — so this is at most one SQLite
    /// read per user action. An id with no local row (not yet synced, or
    /// from a different account) simply has no cache entry; `track_for`
    /// falls back to an honestly-empty stub for it rather than inventing
    /// metadata.
    /// Drops cached metadata for ids no longer in the queue once the cache
    /// has outgrown it by more than a page or so. A rolling-window source
    /// keeps pruning played history (`Queue::prune_history`), so without
    /// this an hours-long shuffle would cache every track it ever played.
    /// Cheap (a length check) except when it actually trims, which is at
    /// most once every `TRACK_CACHE_SLACK` new ids.
    pub(super) fn bound_track_cache(&mut self) {
        const TRACK_CACHE_SLACK: usize = 128;
        if self.track_cache.len() <= self.queue.len() + TRACK_CACHE_SLACK {
            return;
        }
        let keep: std::collections::HashSet<&str> = self.queue.ids().collect();
        self.track_cache.retain(|id, _| keep.contains(id.as_str()));
    }

    pub(super) fn resolve_tracks(&mut self, ids: &[String]) {
        let missing: Vec<String> = ids
            .iter()
            .filter(|id| !self.track_cache.contains_key(id.as_str()))
            .cloned()
            .collect();
        if missing.is_empty() {
            return;
        }
        match library::get_tracks_by_ids(&self.db, &missing) {
            Ok(tracks) => {
                for t in tracks {
                    self.track_cache.insert(t.id.clone(), t);
                }
            }
            Err(e) => tracing::warn!(error = %e.message, "track metadata lookup failed"),
        }
    }

    /// The full `Track` for `id`: real metadata from `track_cache` (the
    /// local library DB) when available, overlaid with whatever the decoder
    /// has actually found for the currently *audible* track (format/
    /// duration) — more precise than the server's metadata (exact bit depth,
    /// trimmed duration) and always real, never fabricated. An id with
    /// neither a cache entry nor live decode info yet (very first tick after
    /// `PlayQueue`, before `resolve_tracks`'s query lands — effectively never
    /// observable since that query runs synchronously in the same command)
    /// falls back to an honestly-empty stub rather than inventing data.
    pub(super) fn track_for(&self, id: &str) -> Track {
        let mut track = self.track_cache.get(id).cloned().unwrap_or_else(|| Track {
            id: id.to_string(),
            title: id.to_string(),
            artist: String::new(),
            artist_id: None,
            album: None,
            album_id: None,
            album_artist: None,
            track_number: None,
            disc_number: None,
            year: None,
            genre: None,
            duration: 0.0,
            format: TrackFormat {
                suffix: String::new(),
                bit_rate: None,
                bit_depth: None,
                sampling_rate: 0,
                channel_count: 2,
            },
            cover_art: None,
            starred: None,
            rating: None,
            size: None,
            replay_gain: None,
            path: None,
            date_added: None,
        });
        if let Some(fmt) = self
            .playback
            .as_ref()
            .filter(|pb| pb.current.id == id)
            .map(|pb| pb.current.format)
        {
            track.format.suffix = fmt.codec_label().to_lowercase();
            track.format.bit_depth = fmt.bit_depth;
            track.format.sampling_rate = fmt.sample_rate;
            track.format.channel_count = fmt.channels;
            let decoded = fmt.duration_sec();
            if decoded > 0.0 {
                track.duration = decoded;
            }
        }
        track
    }

    pub(super) fn player_state(&self) -> PlayerState {
        // While a boundary is pending the queue still points at the audible
        // track, so `current_id` is always what the listener hears.
        let track = self.queue.current_id().map(|id| self.track_for(id));
        let duration = self
            .playback
            .as_ref()
            .map(|pb| pb.current.format.duration_sec())
            .filter(|d| *d > 0.0)
            .or_else(|| track.as_ref().map(|t| t.duration))
            .unwrap_or(0.0);
        let mut position = self.position_sec();
        if duration > 0.0 {
            position = position.min(duration);
        }
        PlayerState {
            status: self.status,
            track,
            position_sec: position,
            duration_sec: duration,
            output_mode: self
                .playback
                .as_ref()
                .map(|pb| pb.negotiated.mode)
                .unwrap_or(self.output_mode_pref),
            sample_rate: self.playback.as_ref().map(|pb| pb.negotiated.sample_rate),
            bit_depth: self
                .playback
                .as_ref()
                .map(|pb| pb.negotiated.container.valid_bits()),
            float_mix: self
                .playback
                .as_ref()
                .is_some_and(|pb| is_float_mix(&pb.negotiated)),
            format_label: self.format_label(),
            bit_perfect: self.is_bit_perfect(),
            volume: self.volume,
            volume_db: output::volume_to_db(self.volume),
            shuffle: self.queue.shuffle,
            repeat: self.queue.repeat,
            queue_length: self.queue.len() as u32,
            error: self.last_error.clone(),
            replay_gain_db: self.applied_replay_gain_db(),
            // Only while the loop still belongs to the current entry (a state
            // built right after Next/Stop, before the next tick clears it).
            loop_a: self.ab_loop.a.filter(|_| self.loop_track.as_deref() == self.queue.current_id()),
            loop_b: self.ab_loop.b.filter(|_| self.loop_track.as_deref() == self.queue.current_id()),
        }
    }
}
