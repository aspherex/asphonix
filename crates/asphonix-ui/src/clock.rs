//! The one clock every animation reads (now-playing bars, hover transitions, spectrum decay): seconds on a
//! process-wide monotonic clock.
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

/// Seconds since the first call.
pub fn now() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}
