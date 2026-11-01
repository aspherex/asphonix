//! The one clock every animation reads (now-playing bars, hover transitions, spectrum decay).
//! In normal builds it is the monotonic clock. With the `parity` feature the test runner can
//! freeze it and step it by hand, so the web and native sides can be sampled at identical
//! animation timestamps.
use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

#[cfg(feature = "parity")]
pub mod manual {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    pub static FROZEN: AtomicBool = AtomicBool::new(false);
    /// Test time in microseconds.
    pub static T_US: AtomicU64 = AtomicU64::new(0);
    pub fn freeze(at_secs: f64) {
        T_US.store((at_secs * 1e6) as u64, Ordering::SeqCst);
        FROZEN.store(true, Ordering::SeqCst);
    }
    pub fn unfreeze() {
        FROZEN.store(false, Ordering::SeqCst);
    }
    pub fn step(dt_secs: f64) {
        T_US.fetch_add((dt_secs * 1e6) as u64, Ordering::SeqCst);
    }
}

/// Seconds since the first call (or the frozen test time).
pub fn now() -> f64 {
    #[cfg(feature = "parity")]
    {
        use std::sync::atomic::Ordering;
        if manual::FROZEN.load(Ordering::Relaxed) {
            return manual::T_US.load(Ordering::SeqCst) as f64 / 1e6;
        }
    }
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// True while the test clock is frozen: animations must then redraw only when the clock moves.
pub fn frozen() -> bool {
    #[cfg(feature = "parity")]
    {
        return manual::FROZEN.load(std::sync::atomic::Ordering::Relaxed);
    }
    #[allow(unreachable_code)]
    false
}
