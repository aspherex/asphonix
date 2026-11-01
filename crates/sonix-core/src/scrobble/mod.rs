pub mod threshold;
const RESTART_EPS_SEC: f64 = 3.0;
pub(crate) fn is_new_play(prev_track_id: &str, prev_position_sec: f64, prev_duration_sec: f64, track_id: &str, position_sec: f64) -> bool {
    if prev_track_id != track_id {
        return true;
    }
    let was_near_end = prev_duration_sec > 0.0 && prev_position_sec >= prev_duration_sec - RESTART_EPS_SEC;
    let now_near_start = position_sec <= RESTART_EPS_SEC;
    was_near_end && now_near_start && position_sec < prev_position_sec
}
