//! ReplayGain ("Volume normalization", opt-in): pure gain math.
//!
//! The tags come from the server (`ipc::ReplayGain`, stored in the local
//! library); this module only turns them into one linear gain factor that the
//! engine multiplies into the user's volume gain, which the render thread
//! already applies in place (`output::apply_gain_in_place`) — so unity (no tag,
//! mode off, or a 0 dB tag) still never touches a sample.


use crate::ipc::ReplayGain;

pub use crate::config::ReplayGainMode;

/// The ReplayGain adjustment to apply for one track, as a linear factor.
/// `None` = unity (mode off, or the track carries no usable tag — honest, we
/// never guess a gain). A tag of the *other* kind is used as a fallback (a
/// track with only an album gain in track mode, and vice versa) since that is
/// still real data from the file; the matching peak is used for the clamp.
///
/// The factor is clamped so the track's own peak can't clip:
/// `gain = min(gain, 1 / peak)` (peak is the linear sample peak, 1.0 =
/// full scale). Returns `(linear, db)` with `db = 20·log10(linear)`.
pub fn replay_gain_factor(mode: ReplayGainMode, rg: Option<&ReplayGain>) -> Option<(f32, f32)> {
    let rg = rg?;
    let (gain_db, peak) = match mode {
        ReplayGainMode::Off => return None,
        ReplayGainMode::Track => match (rg.track_gain_db, rg.album_gain_db) {
            (Some(g), _) => (g, rg.track_peak),
            (None, Some(g)) => (g, rg.album_peak),
            (None, None) => return None,
        },
        ReplayGainMode::Album => match (rg.album_gain_db, rg.track_gain_db) {
            (Some(g), _) => (g, rg.album_peak),
            (None, Some(g)) => (g, rg.track_peak),
            (None, None) => return None,
        },
    };
    if !gain_db.is_finite() {
        return None;
    }
    let mut lin = 10f32.powf(gain_db / 20.0);
    if let Some(p) = peak.filter(|p| p.is_finite() && *p > 0.0) {
        lin = lin.min(1.0 / p);
    }
    if !lin.is_finite() || lin <= 0.0 {
        return None;
    }
    Some((lin, 20.0 * lin.log10()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rg(track: Option<f32>, album: Option<f32>, tp: Option<f32>, ap: Option<f32>) -> ReplayGain {
        ReplayGain { track_gain_db: track, album_gain_db: album, track_peak: tp, album_peak: ap }
    }

    #[test]
    fn off_and_missing_tags_are_unity() {
        let tagged = rg(Some(-6.0), Some(-3.0), None, None);
        assert!(replay_gain_factor(ReplayGainMode::Off, Some(&tagged)).is_none());
        assert!(replay_gain_factor(ReplayGainMode::Track, None).is_none());
        assert!(replay_gain_factor(ReplayGainMode::Album, Some(&rg(None, None, Some(0.9), Some(0.9)))).is_none());
    }

    #[test]
    fn track_and_album_modes_pick_their_own_gain() {
        let tagged = rg(Some(-6.0), Some(-3.0), None, None);
        let (lin, db) = replay_gain_factor(ReplayGainMode::Track, Some(&tagged)).unwrap();
        assert!((db + 6.0).abs() < 1e-4);
        assert!((lin - 0.501_187).abs() < 1e-4);
        let (_, db) = replay_gain_factor(ReplayGainMode::Album, Some(&tagged)).unwrap();
        assert!((db + 3.0).abs() < 1e-4);
    }

    #[test]
    fn falls_back_to_the_other_kind_of_real_tag() {
        let only_album = rg(None, Some(-4.0), None, None);
        assert!((replay_gain_factor(ReplayGainMode::Track, Some(&only_album)).unwrap().1 + 4.0).abs() < 1e-4);
        let only_track = rg(Some(-2.0), None, None, None);
        assert!((replay_gain_factor(ReplayGainMode::Album, Some(&only_track)).unwrap().1 + 2.0).abs() < 1e-4);
    }

    #[test]
    fn positive_gain_is_clamped_by_the_peak_so_it_cannot_clip() {
        // +6 dB would be x1.995, but a 0.8 peak only has x1.25 of headroom.
        let (lin, db) = replay_gain_factor(ReplayGainMode::Track, Some(&rg(Some(6.0), None, Some(0.8), None))).unwrap();
        assert!((lin - 1.25).abs() < 1e-5);
        assert!((db - 20.0 * 1.25f32.log10()).abs() < 1e-4);
        // Album mode uses the album peak, not the track peak.
        let tagged = rg(Some(6.0), Some(6.0), Some(0.5), Some(0.9));
        let (lin, _) = replay_gain_factor(ReplayGainMode::Album, Some(&tagged)).unwrap();
        assert!((lin - 1.0 / 0.9).abs() < 1e-5);
        // Attenuation is never altered by the clamp; a bogus peak is ignored.
        let (lin, db) = replay_gain_factor(ReplayGainMode::Track, Some(&rg(Some(-6.0), None, Some(0.0), None))).unwrap();
        assert!((db + 6.0).abs() < 1e-4 && lin < 1.0);
    }

    #[test]
    fn mode_u8_roundtrip() {
        for m in [ReplayGainMode::Off, ReplayGainMode::Track, ReplayGainMode::Album] {
            assert_eq!(ReplayGainMode::from_u8(m.to_u8()), m);
        }
        assert_eq!(ReplayGainMode::from_u8(200), ReplayGainMode::Off);
    }
}
