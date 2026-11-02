//! Generated: required Backend methods the real backend has not implemented yet (phase 2 steps 2-4).
#![allow(unused_variables)]
use crate::RealBackend;
use sonix_api::*;
impl StarsApi for RealBackend {
    fn star_add(&self, req: StarRequest) -> ApiResult<()> { Err(ApiError::unimplemented("star_add")) }
    fn star_remove(&self, req: StarRequest) -> ApiResult<()> { Err(ApiError::unimplemented("star_remove")) }
}

