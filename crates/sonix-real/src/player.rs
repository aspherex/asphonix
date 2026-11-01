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
