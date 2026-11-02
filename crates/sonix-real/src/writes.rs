//! Library write paths: stars, ratings, server playlists (write-through, then mirror into the local DB),
//! local smart playlists, and the Flow coverage hint.
//! Ports `ipc.rs` (playlists_*, star_*, rating_set), `smart_ipc.rs` and `flow_ipc.rs` of the WebView app.
use crate::reads::conv;
use crate::{err, RealBackend};
use sonix_api::*;
use sonix_core::audio::EngineCommand;
use sonix_core::ipc as core;
use sonix_core::listen::{spawn_feedback, FeedbackKind};
use sonix_core::subsonic::{Client, StarKind};
use sonix_core::{library, smartlists};

fn not_connected() -> ApiError {
    ApiError::new(ErrorCode::NotConnected, "not connected to a server")
}

fn fresh_seed() -> u64 {
    use rand::Rng;
    rand::thread_rng().gen_range(0..4_294_967_296u64)
}

impl RealBackend {
    fn require_client(&self) -> ApiResult<Client> {
        self.0.client.read().unwrap_or_else(|p| p.into_inner()).clone().ok_or_else(not_connected)
    }

    fn current_username(&self) -> Option<String> {
        self.0.session.lock().unwrap().cfg.as_ref().map(|c| c.username.clone())
    }

    /// Smart/read-only flags (`ipc::apply_playlist_access`).
    fn access(&self, p: &mut core::Playlist) {
        let me = self.current_username();
        let (smart, read_only) = library::playlist_access_flags(p.server_read_only, p.owner.as_deref(), me.as_deref());
        p.smart = smart;
        p.read_only = read_only;
    }

    /// Backend guard behind the UI's disabled controls (`ipc::ensure_playlist_writable`).
    fn ensure_writable(&self, id: &str) -> ApiResult<()> {
        if let Some((owner, server_ro)) = library::playlist_access(&self.0.db, id).map_err(err)? {
            let me = self.current_username();
            let (_, read_only) = library::playlist_access_flags(server_ro, owner.as_deref(), me.as_deref());
            if read_only {
                return Err(ApiError::new(ErrorCode::ServerError, "this playlist is read-only (smart or owned by another user)"));
            }
        }
        Ok(())
    }

    /// Re-fetches the authoritative playlist from the server into the local mirror.
    async fn refresh_playlist(client: &Client, db: &library::Db, id: &str) -> Result<(), core::IpcError> {
        let detail = client.get_playlist(id).await?;
        let playlist = library::playlist_from_raw(&detail.playlist);
        library::upsert_playlist_meta(db, &playlist)?;
        let track_ids: Vec<String> = detail.entry.iter().map(|s| s.id.clone()).collect();
        library::set_playlist_tracks_local(db, id, &track_ids)
    }

    fn changed(&self) {
        self.emit(Event::LibraryChanged);
    }
}

impl PlaylistsApi for RealBackend {
    fn playlists_list(&self) -> ApiResult<Vec<Playlist>> {
        let mut lists = library::playlists_list(&self.0.db).map_err(err)?;
        lists.iter_mut().for_each(|p| self.access(p));
        conv(lists)
    }

    fn playlists_get(&self, id: &str) -> ApiResult<PlaylistDetail> {
        let mut d = library::playlist_detail(&self.0.db, id).map_err(err)?.ok_or_else(|| ApiError::new(ErrorCode::NotFound, format!("playlist {id} not found")))?;
        self.access(&mut d.playlist);
        conv(d)
    }

    fn playlists_create(&self, req: CreatePlaylistRequest) -> ApiResult<Playlist> {
        let client = self.require_client()?;
        let db = &self.0.db;
        let mut playlist = self
            .0
            .rt
            .block_on(async {
                let raw = client.create_playlist(&req.name).await?;
                let mut playlist = library::playlist_from_raw(&raw);
                library::upsert_playlist_meta(db, &playlist)?;
                // The native UI can create from a selection (`trackIds`); the WebView command only takes a name.
                if !req.track_ids.is_empty() {
                    client.update_playlist_add_songs(&raw.id, &req.track_ids).await?;
                    Self::refresh_playlist(&client, db, &raw.id).await?;
                    if let Some(d) = library::playlist_detail(db, &raw.id)? {
                        playlist = d.playlist;
                    }
                }
                Ok::<_, core::IpcError>(playlist)
            })
            .map_err(err)?;
        self.access(&mut playlist);
        self.changed();
        conv(playlist)
    }

    fn playlists_rename(&self, req: RenamePlaylistRequest) -> ApiResult<()> {
        self.ensure_writable(&req.id)?;
        let client = self.require_client()?;
        self.0.rt.block_on(client.update_playlist_rename(&req.id, &req.name)).map_err(err)?;
        library::rename_playlist_local(&self.0.db, &req.id, &req.name).map_err(err)?;
        self.changed();
        Ok(())
    }

    fn playlists_delete(&self, req: DeletePlaylistRequest) -> ApiResult<()> {
        self.ensure_writable(&req.id)?;
        let client = self.require_client()?;
        self.0.rt.block_on(client.delete_playlist(&req.id)).map_err(err)?;
        library::delete_playlist_local(&self.0.db, &req.id).map_err(err)?;
        self.changed();
        Ok(())
    }

    fn playlists_add_tracks(&self, req: AddToPlaylistRequest) -> ApiResult<()> {
        self.ensure_writable(&req.id)?;
        let client = self.require_client()?;
        self.0
            .rt
            .block_on(async {
                client.update_playlist_add_songs(&req.id, &req.track_ids).await?;
                Self::refresh_playlist(&client, &self.0.db, &req.id).await
            })
            .map_err(err)?;
        self.changed();
        Ok(())
    }

    fn playlists_remove_tracks(&self, req: RemovePlaylistTracksRequest) -> ApiResult<()> {
        self.ensure_writable(&req.id)?;
        let client = self.require_client()?;
        let indices: Vec<u32> = req.indices.iter().map(|i| *i as u32).collect();
        self.0
            .rt
            .block_on(async {
                client.update_playlist_remove_indices(&req.id, &indices).await?;
                Self::refresh_playlist(&client, &self.0.db, &req.id).await
            })
            .map_err(err)?;
        self.changed();
        Ok(())
    }

    /// `updatePlaylist` has no reorder primitive: remove every entry and re-add the full list in ONE request.
    fn playlists_reorder(&self, req: ReorderPlaylistRequest) -> ApiResult<()> {
        self.ensure_writable(&req.id)?;
        let client = self.require_client()?;
        self.0
            .rt
            .block_on(async {
                let current = client.get_playlist(&req.id).await?;
                let indices: Vec<u32> = (0..current.entry.len() as u32).rev().collect();
                client.update_playlist_replace(&req.id, &indices, &req.track_ids).await?;
                Self::refresh_playlist(&client, &self.0.db, &req.id).await
            })
            .map_err(err)?;
        self.changed();
        Ok(())
    }

    fn playlists_duplicate(&self, id: &str) -> ApiResult<Playlist> {
        let client = self.require_client()?;
        let db = &self.0.db;
        let mut playlist = self
            .0
            .rt
            .block_on(async {
                let source = client.get_playlist(id).await?;
                let created = client.create_playlist(&format!("{} (copy)", source.playlist.name)).await?;
                let track_ids: Vec<String> = source.entry.iter().map(|s| s.id.clone()).collect();
                client.update_playlist_add_songs(&created.id, &track_ids).await?;
                Self::refresh_playlist(&client, db, &created.id).await?;
                library::playlist_detail(db, &created.id)?.map(|d| d.playlist).ok_or_else(|| core::IpcError {
                    code: core::IpcErrorCode::ServerError,
                    message: "playlists_duplicate: new playlist missing from local mirror after refresh".into(),
                })
            })
            .map_err(err)?;
        self.access(&mut playlist);
        self.changed();
        conv(playlist)
    }
}

fn star_kinds(t: StarTargetType) -> (StarKind, library::StarKindLocal, library::RatingKindLocal) {
    match t {
        StarTargetType::Track => (StarKind::Track, library::StarKindLocal::Track, library::RatingKindLocal::Track),
        StarTargetType::Album => (StarKind::Album, library::StarKindLocal::Album, library::RatingKindLocal::Album),
        StarTargetType::Artist => (StarKind::Artist, library::StarKindLocal::Artist, library::RatingKindLocal::Artist),
    }
}

impl StarsApi for RealBackend {
    fn star_add(&self, req: StarRequest) -> ApiResult<()> {
        let client = self.require_client()?;
        let (remote, local, _) = star_kinds(req.kind);
        self.0.rt.block_on(client.star(&req.id, remote)).map_err(err)?;
        let now = chrono::Utc::now().to_rfc3339();
        library::set_starred_local(&self.0.db, &req.id, local, Some(now.clone())).map_err(err)?;
        if req.kind == StarTargetType::Track {
            spawn_feedback(&self.0.db, &req.id, FeedbackKind::Like, None);
            self.0.engine.send(EngineCommand::UpdateTrackMeta { id: req.id, starred: Some(Some(now)), rating: None });
        }
        self.changed();
        Ok(())
    }

    fn star_remove(&self, req: StarRequest) -> ApiResult<()> {
        let client = self.require_client()?;
        let (remote, local, _) = star_kinds(req.kind);
        self.0.rt.block_on(client.unstar(&req.id, remote)).map_err(err)?;
        library::set_starred_local(&self.0.db, &req.id, local, None).map_err(err)?;
        if req.kind == StarTargetType::Track {
            spawn_feedback(&self.0.db, &req.id, FeedbackKind::Unlike, None);
            self.0.engine.send(EngineCommand::UpdateTrackMeta { id: req.id, starred: Some(None), rating: None });
        }
        self.changed();
        Ok(())
    }

    fn rating_set(&self, req: SetRatingRequest) -> ApiResult<()> {
        if req.rating > 5 {
            return Err(ApiError::new(ErrorCode::Unknown, format!("rating {} out of range (0-5)", req.rating)));
        }
        let client = self.require_client()?;
        let (_, _, local) = star_kinds(req.kind);
        self.0.rt.block_on(client.set_rating(&req.id, req.rating)).map_err(err)?;
        library::set_rating_local(&self.0.db, &req.id, local, req.rating).map_err(err)?;
        if req.kind == StarTargetType::Track {
            spawn_feedback(&self.0.db, &req.id, FeedbackKind::Rating, Some(i64::from(req.rating)));
            self.0.engine.send(EngineCommand::UpdateTrackMeta { id: req.id, starred: None, rating: Some((req.rating > 0).then_some(req.rating)) });
        }
        self.changed();
        Ok(())
    }
}

// --- smart playlists (local JSON store) + Flow coverage -------------------------------------------
// The trait methods live in `reads.rs` (LibraryApi) and `player.rs` (PlayerApi) and delegate here.

impl RealBackend {
    fn smart_summary(&self, p: smartlists::SmartPlaylist, seed: u64) -> ApiResult<SmartSummary> {
        let (track_count, duration) = smartlists::summarize(&self.0.db, &p.def, seed).map_err(err)?;
        conv(smartlists::SmartSummary { playlist: p, track_count, duration })
    }

    pub(crate) fn smart_list_impl(&self) -> ApiResult<Vec<SmartSummary>> {
        self.0.smart.list().map_err(err)?.into_iter().map(|p| self.smart_summary(p, 0)).collect()
    }
    pub(crate) fn smart_get_impl(&self, id: &str, seed: u32) -> ApiResult<SmartSummary> {
        let p = self.0.smart.get(id).map_err(err)?;
        self.smart_summary(p, u64::from(seed))
    }
    pub(crate) fn smart_create_impl(&self, input: SmartInput) -> ApiResult<SmartSummary> {
        let p = self.0.smart.create(conv(input)?).map_err(err)?;
        self.smart_summary(p, 0)
    }
    pub(crate) fn smart_update_impl(&self, id: &str, input: SmartInput) -> ApiResult<SmartSummary> {
        let p = self.0.smart.update(id, conv(input)?).map_err(err)?;
        self.smart_summary(p, 0)
    }
    pub(crate) fn smart_delete_impl(&self, id: &str) -> ApiResult<()> {
        self.0.smart.delete(id).map_err(err)
    }
    pub(crate) fn smart_duplicate_impl(&self, id: &str) -> ApiResult<SmartSummary> {
        let p = self.0.smart.duplicate(id).map_err(err)?;
        self.smart_summary(p, 0)
    }
    pub(crate) fn smart_tracks_impl(&self, id: &str, seed: u32, req: PageRequest) -> ApiResult<Paged<Track>> {
        let p = self.0.smart.get(id).map_err(err)?;
        conv(smartlists::evaluate_page(&self.0.db, &p.def, req.offset as u64, (req.limit as u64).clamp(1, 500), u64::from(seed)).map_err(err)?)
    }
    pub(crate) fn smart_preview_impl(&self, input: SmartInput, seed: u32, req: PageRequest) -> ApiResult<Paged<Track>> {
        let def: smartlists::SmartInput = conv(input)?;
        conv(smartlists::evaluate_page(&self.0.db, &def, req.offset as u64, (req.limit as u64).clamp(1, 500), u64::from(seed)).map_err(err)?)
    }

    /// `smart_source_cmd`: seed 0 means "none given" (fresh random seed), like the optional wire field.
    pub(crate) fn play_smart_impl(&self, req: PlaySourceRequest) -> ApiResult<()> {
        let p = self.0.smart.get(&req.id).map_err(err)?;
        let seed = if req.seed == 0 { fresh_seed() } else { u64::from(req.seed) };
        let rows = smartlists::play_ids(&self.0.db, &p.def, req.start_track_id.as_deref(), seed).map_err(err)?;
        if rows.is_empty() {
            return Err(ApiError::not_found("matching tracks in this smart playlist"));
        }
        self.play_rows(core::SourceKind::Smart, p.def.name.clone(), Some(req.id), rows, req.shuffle, req.flow, req.start_track_id)
    }

    /// `flow_get_coverage`: real coverage over the exact pool a Flow play of the source would use.
    pub(crate) fn flow_coverage_impl(&self, req: FlowCoverageRequest) -> ApiResult<FlowCoverage> {
        let db = &self.0.db;
        let need_id = || req.id.as_deref().ok_or_else(|| ApiError::new(ErrorCode::Unknown, "id required"));
        let rows: Vec<(String, String)> = match req.kind {
            FlowPoolKind::Playlist => library::get_playlist_play_source(db, need_id()?, None).map_err(err)?.ok_or_else(|| ApiError::not_found("playlist"))?.1,
            FlowPoolKind::Artist => library::get_artist_play_source(db, need_id()?, None).map_err(err)?.ok_or_else(|| ApiError::not_found("artist"))?.1,
            FlowPoolKind::Genre => library::discovery::get_genre_play_source(db, need_id()?, None).map_err(err)?.ok_or_else(|| ApiError::not_found("genre"))?.1,
            FlowPoolKind::Liked => library::get_liked_play_source(db, None).map_err(err)?,
            FlowPoolKind::Smart => {
                let p = self.0.smart.get(need_id()?).map_err(err)?;
                smartlists::play_ids(db, &p.def, None, u64::from(req.seed.unwrap_or(0))).map_err(err)?
            }
            FlowPoolKind::AllTracks => library::get_all_track_ids(db, core::TrackSortField::Title, core::SortDir::Asc, req.filter.as_deref(), None).map_err(err)?,
        };
        let mut seen = std::collections::HashSet::with_capacity(rows.len());
        let ids: Vec<String> = rows.into_iter().map(|(id, _)| id).filter(|id| seen.insert(id.clone())).collect();
        let feats = library::flow::load_flow_features(db, &ids).map_err(err)?;
        let empty = sonix_core::audio::sequence::FlowTrack::default();
        conv(sonix_core::audio::sequence::coverage(ids.iter().map(|id| feats.get(id).unwrap_or(&empty))))
    }
}

#[cfg(test)]
#[path = "writes_tests.rs"]
mod tests;
