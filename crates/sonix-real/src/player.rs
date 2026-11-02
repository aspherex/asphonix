//! Playback over the vendored audio engine (WASAPI/cpal output, queue, gapless, Flow...).
use crate::reads::conv;
use crate::{err, RealBackend};
use sonix_api::*;
use sonix_core::audio::{EngineCommand, EngineEvents};
use sonix_core::ipc as core;
use sonix_core::library;

/// Engine events -> UI events.
pub(crate) struct Events(pub std::sync::mpsc::Sender<Event>);

impl EngineEvents for Events {
    fn player_state(&self, state: &core::PlayerState) {
        if let Ok(s) = conv::<_, PlayerState>(state) {
            let _ = self.0.send(Event::PlayerState(Box::new(s)));
        }
    }
    fn spectrum(&self, frame: core::SpectrumFrame) {
        if let Ok(f) = conv::<_, SpectrumFrame>(frame) {
            let _ = self.0.send(Event::Spectrum(f));
        }
    }
}

impl RealBackend {
    fn send(&self, c: EngineCommand) -> ApiResult<()> {
        self.0.engine.send(c);
        Ok(())
    }
}

impl PlayerApi for RealBackend {
    fn player_play_queue(&self, req: PlayQueueRequest) -> ApiResult<()> {
        self.send(EngineCommand::PlayQueue { track_ids: req.track_ids, start_index: req.start_index as u32 })
    }
    fn player_play_all(&self, req: PlayAllRequest) -> ApiResult<()> {
        let q: core::PlayAllRequest = conv(req)?;
        let (sort, dir, shuffle) = match q.order {
            core::TrackOrder::Shuffle => (core::TrackSortField::Title, core::SortDir::Asc, true),
            core::TrackOrder::Sort { field, dir } => (field, dir, false),
        };
        let rows = library::get_all_track_ids(&self.0.db, sort, dir, q.filter.as_deref(), q.start_track_id.as_deref()).map_err(err)?;
        let (track_ids, artists): (Vec<String>, Vec<Option<String>>) = rows.into_iter().map(|(id, a)| (id, (!a.is_empty()).then_some(a))).unzip();
        self.send(EngineCommand::PlaySource {
            kind: core::SourceKind::AllTracks,
            label: "All Tracks".into(),
            source_id: None,
            mode: if shuffle { core::SourceMode::Shuffle } else { core::SourceMode::InOrder },
            arc: Default::default(),
            track_ids,
            artists,
            start_track_id: q.start_track_id,
        })
    }
    fn player_play_playlist(&self, req: PlaySourceRequest) -> ApiResult<()> {
        self.play_playlist_impl(req)
    }
    fn player_play_liked(&self, req: PlaySourceRequest) -> ApiResult<()> {
        self.play_liked_impl(req)
    }
    fn player_play_smart(&self, req: PlaySourceRequest) -> ApiResult<()> {
        self.play_smart_impl(req)
    }
    fn flow_get_coverage(&self, req: FlowCoverageRequest) -> ApiResult<FlowCoverage> {
        self.flow_coverage_impl(req)
    }
    fn player_play_artist(&self, req: PlayArtistRequest) -> ApiResult<()> {
        self.play_artist_impl(req)
    }
    fn player_get_waveform(&self, track_id: &str) -> ApiResult<Waveform> {
        self.waveform_impl(track_id)
    }
    fn lyrics_get(&self, track_id: &str) -> ApiResult<Option<Lyrics>> {
        self.lyrics_impl(track_id)
    }
    fn player_loop_cycle(&self) -> ApiResult<()> {
        self.loop_cycle_impl()
    }
    fn radio_start(&self, req: RadioStartRequest) -> ApiResult<()> {
        self.radio_start_impl(req)
    }
    fn player_play_genre(&self, req: PlayGenreRequest) -> ApiResult<()> {
        self.play_genre_impl(req)
    }
    fn player_play_pause(&self) -> ApiResult<()> {
        self.send(EngineCommand::PlayPause)
    }
    fn player_play(&self) -> ApiResult<()> {
        self.send(EngineCommand::Play)
    }
    fn player_pause(&self) -> ApiResult<()> {
        self.send(EngineCommand::Pause)
    }
    fn player_next(&self) -> ApiResult<()> {
        self.send(EngineCommand::Next)
    }
    fn player_previous(&self) -> ApiResult<()> {
        self.send(EngineCommand::Previous)
    }
    fn updates_check(&self) -> ApiResult<UpdateInfo> {
        self.updates_check_impl()
    }
    fn updates_install(&self) -> ApiResult<()> {
        self.updates_install_impl()
    }
    fn player_seek(&self, req: SeekRequest) -> ApiResult<()> {
        self.send(EngineCommand::Seek(req.position_sec))
    }
    fn player_get_state(&self) -> ApiResult<PlayerState> {
        conv(self.0.engine.last_player_state())
    }
    fn player_set_spectrum_active(&self, active: bool) -> ApiResult<()> {
        self.0.prefs.set_spectrum_active(active);
        Ok(())
    }
    fn player_set_volume(&self, req: SetVolumeRequest) -> ApiResult<()> {
        self.send(EngineCommand::SetVolume(req.volume as f32))
    }
    fn player_set_shuffle(&self, enabled: bool) -> ApiResult<()> {
        self.send(EngineCommand::SetShuffle(enabled))
    }
    fn player_set_repeat(&self, mode: RepeatMode) -> ApiResult<()> {
        self.send(EngineCommand::SetRepeat(conv(mode)?))
    }
    fn player_get_queue(&self, req: PageRequest) -> ApiResult<QueueState> {
        let q = self
            .0
            .engine
            .request_blocking(|reply| EngineCommand::GetQueue { offset: req.offset as u32, limit: req.limit as u32, reply })
            .ok_or_else(|| ApiError::new(ErrorCode::Unknown, "engine did not reply"))?;
        conv(q)
    }
    fn player_jump_to(&self, index: usize) -> ApiResult<()> {
        self.send(EngineCommand::JumpTo(index as u32))
    }
    fn player_enqueue(&self, req: EnqueueRequest) -> ApiResult<()> {
        self.send(EngineCommand::Enqueue { track_ids: req.track_ids, position: conv(req.position)? })
    }
    fn player_remove_from_queue(&self, index: usize) -> ApiResult<()> {
        self.send(EngineCommand::RemoveFromQueue(index as u32))
    }
    fn player_reorder_queue(&self, from: usize, to: usize) -> ApiResult<()> {
        self.send(EngineCommand::ReorderQueue { from_index: from as u32, to_index: to as u32 })
    }
}

impl OutputApi for RealBackend {
    fn output_list_devices(&self) -> ApiResult<Vec<OutputDevice>> {
        conv(self.0.engine.request_blocking(EngineCommand::ListOutputDevices).unwrap_or_default())
    }
    fn output_set_device(&self, id: &str) -> ApiResult<()> {
        self.send(EngineCommand::SetOutputDevice(id.to_string()))
    }
    fn output_set_mode(&self, mode: OutputMode) -> ApiResult<()> {
        self.send(EngineCommand::SetOutputMode(conv(mode)?))
    }
    fn output_get_status(&self) -> ApiResult<OutputStatus> {
        let s = self.0.engine.request_blocking(EngineCommand::GetOutputStatus).ok_or_else(|| ApiError::new(ErrorCode::Unknown, "engine did not reply"))?;
        conv(s)
    }
}

/// How a row source plays (the `shuffle` / `flow` / `startTrackId` fields every play request carries).
pub(crate) struct PlayOpts {
    pub shuffle: bool,
    pub flow: Option<FlowArc>,
    pub start: Option<String>,
}

impl RealBackend {
    /// Rolling `PlaySource` from `(id, artist)` rows (playlist/artist/genre/liked), like the WebView app's `*_source_cmd`.
    pub(crate) fn play_rows(&self, kind: core::SourceKind, label: String, source_id: Option<String>, rows: Vec<(String, String)>, opts: PlayOpts) -> ApiResult<()> {
        let PlayOpts { shuffle, flow, start } = opts;
        let (track_ids, artists): (Vec<String>, Vec<Option<String>>) = rows.into_iter().map(|(id, a)| (id, (!a.is_empty()).then_some(a))).unzip();
        let (mode, arc) = match flow {
            Some(f) => (core::SourceMode::Flow, conv(f)?),
            None if shuffle => (core::SourceMode::Shuffle, Default::default()),
            None => (core::SourceMode::InOrder, Default::default()),
        };
        self.send(EngineCommand::PlaySource { kind, label, source_id, mode, arc, track_ids, artists, start_track_id: start })
    }
}

impl RealBackend {
    pub(crate) fn play_playlist_impl(&self, req: PlaySourceRequest) -> ApiResult<()> {
        let (name, rows) = library::get_playlist_play_source(&self.0.db, &req.id, req.start_track_id.as_deref()).map_err(err)?.ok_or_else(|| ApiError::not_found("playlist"))?;
        self.play_rows(core::SourceKind::Playlist, name, Some(req.id), rows, PlayOpts { shuffle: req.shuffle, flow: req.flow, start: req.start_track_id })
    }
    pub(crate) fn play_liked_impl(&self, req: PlaySourceRequest) -> ApiResult<()> {
        let rows = library::get_liked_play_source(&self.0.db, req.start_track_id.as_deref()).map_err(err)?;
        self.play_rows(core::SourceKind::Liked, "Liked Songs".into(), None, rows, PlayOpts { shuffle: req.shuffle, flow: req.flow, start: req.start_track_id })
    }
    pub(crate) fn play_artist_impl(&self, req: PlayArtistRequest) -> ApiResult<()> {
        let (name, rows) = library::get_artist_play_source(&self.0.db, &req.id, req.start_track_id.as_deref()).map_err(err)?.ok_or_else(|| ApiError::not_found("artist"))?;
        self.play_rows(core::SourceKind::Artist, name, Some(req.id), rows, PlayOpts { shuffle: req.shuffle, flow: req.flow, start: req.start_track_id })
    }
    pub(crate) fn play_genre_impl(&self, req: PlayGenreRequest) -> ApiResult<()> {
        let (name, rows) = library::discovery::get_genre_play_source(&self.0.db, &req.name, req.start_track_id.as_deref()).map_err(err)?.ok_or_else(|| ApiError::not_found("genre"))?;
        self.play_rows(core::SourceKind::Genre, name, None, rows, PlayOpts { shuffle: req.shuffle, flow: req.flow, start: req.start_track_id })
    }
}
