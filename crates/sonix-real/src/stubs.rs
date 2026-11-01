//! Generated: required Backend methods the real backend has not implemented yet (phase 2 steps 2-4).
#![allow(unused_variables)]
use crate::RealBackend;
use sonix_api::*;
impl StarsApi for RealBackend {
    fn star_add(&self, req: StarRequest) -> ApiResult<()> { Err(ApiError::unimplemented("star_add")) }
    fn star_remove(&self, req: StarRequest) -> ApiResult<()> { Err(ApiError::unimplemented("star_remove")) }
}
impl PlayerApi for RealBackend {
    fn player_play_queue(&self, req: PlayQueueRequest) -> ApiResult<()> { Err(ApiError::unimplemented("player_play_queue")) }
    fn player_play_all(&self, req: PlayAllRequest) -> ApiResult<()> { Err(ApiError::unimplemented("player_play_all")) }
    fn player_play_pause(&self) -> ApiResult<()> { Err(ApiError::unimplemented("player_play_pause")) }
    fn player_play(&self) -> ApiResult<()> { Err(ApiError::unimplemented("player_play")) }
    fn player_pause(&self) -> ApiResult<()> { Err(ApiError::unimplemented("player_pause")) }
    fn player_next(&self) -> ApiResult<()> { Err(ApiError::unimplemented("player_next")) }
    fn player_previous(&self) -> ApiResult<()> { Err(ApiError::unimplemented("player_previous")) }
    fn player_seek(&self, req: SeekRequest) -> ApiResult<()> { Err(ApiError::unimplemented("player_seek")) }
    fn player_get_state(&self) -> ApiResult<PlayerState> { Err(ApiError::unimplemented("player_get_state")) }
    fn player_set_spectrum_active(&self, active: bool) -> ApiResult<()> { Err(ApiError::unimplemented("player_set_spectrum_active")) }
    fn player_set_volume(&self, req: SetVolumeRequest) -> ApiResult<()> { Err(ApiError::unimplemented("player_set_volume")) }
    fn player_set_shuffle(&self, enabled: bool) -> ApiResult<()> { Err(ApiError::unimplemented("player_set_shuffle")) }
    fn player_set_repeat(&self, mode: RepeatMode) -> ApiResult<()> { Err(ApiError::unimplemented("player_set_repeat")) }
    fn player_get_queue(&self, req: PageRequest) -> ApiResult<QueueState> { Err(ApiError::unimplemented("player_get_queue")) }
    fn player_jump_to(&self, index: usize) -> ApiResult<()> { Err(ApiError::unimplemented("player_jump_to")) }
    fn player_enqueue(&self, req: EnqueueRequest) -> ApiResult<()> { Err(ApiError::unimplemented("player_enqueue")) }
}
impl OutputApi for RealBackend {
    fn output_get_status(&self) -> ApiResult<OutputStatus> { Err(ApiError::unimplemented("output_get_status")) }
}
impl ArtApi for RealBackend {
    fn art_get(&self, cover_art: &str, size: u32) -> ApiResult<ArtPixels> { Err(ApiError::unimplemented("art_get")) }
}
impl SettingsApi for RealBackend {}
