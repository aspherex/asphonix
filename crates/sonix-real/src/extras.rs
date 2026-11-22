//! Read-only discovery and player extras on `RealBackend`: waveform, lyrics, A-B loop, artist top tracks/info, radio.
//! Logic lives in `sonix_core::extras`; these are thin adapters.
use crate::reads::conv;
use crate::{err, RealBackend};
use sonix_api::*;
use sonix_core::audio::EngineCommand;
use sonix_core::extras;
use sonix_core::ipc as core;
use sonix_core::library;

impl RealBackend {
    fn client_now(&self) -> Option<sonix_core::subsonic::Client> {
        self.0.client.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Waveform as far as known; the first call for a track starts the background scan (poll until `done`).
    pub(crate) fn waveform_impl(&self, track_id: &str) -> ApiResult<Waveform> {
        // Library duration is only the fallback for containers without a frame count.
        let hint = library::get_tracks_by_ids(&self.0.db, std::slice::from_ref(&track_id.to_string()))
            .map_err(err)?
            .first()
            .map(|t| t.duration)
            .unwrap_or(0.0);
        let s = self.0.waveform.get(track_id, hint);
        Ok(Waveform { track_id: s.track_id, buckets: s.buckets, filled: s.filled, done: s.done })
    }

    pub(crate) fn lyrics_impl(&self, track_id: &str) -> ApiResult<Option<Lyrics>> {
        let l = self.0.rt.block_on(extras::lyrics_get(&self.0.db, self.client_now(), track_id)).map_err(err)?;
        l.map(conv).transpose()
    }

    pub(crate) fn loop_cycle_impl(&self) -> ApiResult<()> {
        self.0.engine.send(EngineCommand::LoopCycle);
        Ok(())
    }

    pub(crate) fn top_tracks_impl(&self, id: &str) -> ApiResult<ArtistTopTracks> {
        conv(self.0.rt.block_on(sonix_core::extras::artist_top_tracks(&self.0.db, self.client_now(), id)).map_err(err)?)
    }

    pub(crate) fn artist_info_impl(&self, id: &str) -> ApiResult<ArtistInfo> {
        conv(self.0.rt.block_on(sonix_core::extras::artist_info(&self.0.db, self.client_now(), id)).map_err(err)?)
    }

    /// Radio queue: similar tracks (server, then local top-up), played as an in-order `radio` source. Read-only.
    pub(crate) fn radio_start_impl(&self, req: RadioStartRequest) -> ApiResult<()> {
        let client = self.client_now().ok_or_else(|| ApiError::new(ErrorCode::NotConnected, "not connected to a server"))?;
        let kind = match req.seed.kind {
            RadioSeedKind::Track => core::StarTargetType::Track,
            RadioSeedKind::Album => core::StarTargetType::Album,
            RadioSeedKind::Artist => core::StarTargetType::Artist,
        };
        let (track_ids, artists) = self.0.rt.block_on(sonix_core::continuity::radio_pool(&client, &self.0.db, kind, &req.seed.id)).map_err(err)?;
        if track_ids.is_empty() {
            return Err(err(extras::no_similar()));
        }
        self.0.engine.send(EngineCommand::PlaySource {
            kind: core::SourceKind::Radio,
            label: "Radio".into(),
            source_id: None,
            mode: core::SourceMode::InOrder,
            arc: core::FlowArc::Steady,
            track_ids,
            artists,
            start_track_id: None,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests;
