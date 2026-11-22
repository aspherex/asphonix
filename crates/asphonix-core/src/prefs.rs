//! Live playback preferences shared between the settings commands and the
//! engine thread: plain atomics, so a toggle is visible to the engine on its
//! next loop iteration without a command round-trip (and without spawning the
//! lazily started engine thread just to store a flag).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

use crate::config::ReplayGainMode;

pub struct Prefs {
    autoplay: AtomicBool,
    sync_play_queue: AtomicBool,
    replay_gain: AtomicU8,
    /// Whether any view currently shows the spectrum/peak meters. Shared
    /// with the render callbacks and the spectrum thread (they skip the
    /// downmix / FFT / emit while false). Off until a subscriber appears.
    spectrum_active: Arc<AtomicBool>,
    /// `Settings.playThresholdSec`, mirrored here so the engine-side listen
    /// recorder (`listen::`) applies the same "counted" rule as `PlayCounter`.
    play_threshold_sec: AtomicU32,
}

impl Default for Prefs {
    /// Same defaults as `secrets::Settings`: autoplay OFF, server queue sync OFF (network-visible features
    /// are opt-in; a careless `Prefs::default()` must never opt in),
    /// ReplayGain off.
    fn default() -> Self {
        Self::new(false, false, ReplayGainMode::Off)
    }
}

impl Prefs {
    pub fn new(autoplay: bool, sync_play_queue: bool, replay_gain: ReplayGainMode) -> Self {
        Self {
            autoplay: AtomicBool::new(autoplay),
            sync_play_queue: AtomicBool::new(sync_play_queue),
            replay_gain: AtomicU8::new(replay_gain.to_u8()),
            spectrum_active: Arc::new(AtomicBool::new(false)),
            play_threshold_sec: AtomicU32::new(crate::config::DEFAULT_PLAY_THRESHOLD_SEC),
        }
    }

    pub fn play_threshold_sec(&self) -> u32 {
        self.play_threshold_sec.load(Ordering::Relaxed)
    }

    pub fn set_play_threshold_sec(&self, v: u32) {
        self.play_threshold_sec.store(v, Ordering::Relaxed);
    }

    pub fn autoplay(&self) -> bool {
        self.autoplay.load(Ordering::Relaxed)
    }

    pub fn set_autoplay(&self, v: bool) {
        self.autoplay.store(v, Ordering::Relaxed);
    }

    pub fn sync_play_queue(&self) -> bool {
        self.sync_play_queue.load(Ordering::Relaxed)
    }

    pub fn set_sync_play_queue(&self, v: bool) {
        self.sync_play_queue.store(v, Ordering::Relaxed);
    }

    pub fn spectrum_active(&self) -> Arc<AtomicBool> {
        self.spectrum_active.clone()
    }

    pub fn set_spectrum_active(&self, v: bool) {
        self.spectrum_active.store(v, Ordering::Relaxed);
    }

    pub fn replay_gain(&self) -> ReplayGainMode {
        ReplayGainMode::from_u8(self.replay_gain.load(Ordering::Relaxed))
    }

    pub fn set_replay_gain(&self, m: ReplayGainMode) {
        self.replay_gain.store(m.to_u8(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectrum_is_inactive_until_asked_and_the_flag_is_shared() {
        let p = Prefs::default();
        let flag = p.spectrum_active();
        assert!(!flag.load(Ordering::Relaxed));
        p.set_spectrum_active(true);
        assert!(flag.load(Ordering::Relaxed));
        p.set_spectrum_active(false);
        assert!(!flag.load(Ordering::Relaxed));
    }
}
