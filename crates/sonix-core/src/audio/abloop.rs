//! A-B loop state machine (Now Playing's loop button). Pure logic, no I/O:
//! the engine feeds it the audible position and performs the actual seek
//! (`Engine::seek`, the in-place path) when [`AbLoop::due`] says so, so the
//! loop is exact to the engine tick rather than to a UI timer.
//!
//! One button cycles: set A at the current position -> set B -> clear.
//! Cleared on every track change (the engine compares the audible id).

/// B must be at least this far past A, otherwise the second press would
/// create a loop too short to hear (or a zero-length one that seeks forever).
pub const MIN_LOOP_SEC: f64 = 0.5;
/// A position this far past B is treated as an explicit seek out of the loop
/// (the user dragged the seek bar beyond it), not as reaching B: no jump.
const ESCAPE_SEC: f64 = 1.5;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AbLoop {
    pub a: Option<f64>,
    pub b: Option<f64>,
}

impl AbLoop {
    /// One press of the loop button at audible position `pos`.
    pub fn cycle(&mut self, pos: f64) {
        match (self.a, self.b) {
            (None, _) => {
                self.a = Some(pos.max(0.0));
                self.b = None;
            }
            (Some(a), None) => {
                let pos = pos.max(0.0);
                if pos >= a + MIN_LOOP_SEC {
                    self.b = Some(pos);
                } else if pos + MIN_LOOP_SEC <= a {
                    // The second press is *before* A (the user seeked back):
                    // the two marks still define a loop, earlier one first.
                    self.b = Some(a);
                    self.a = Some(pos);
                }
                // Within MIN_LOOP_SEC of A: keep waiting for a usable B.
            }
            (Some(_), Some(_)) => self.clear(),
        }
    }

    pub fn clear(&mut self) {
        self.a = None;
        self.b = None;
    }

    pub fn is_active(&self) -> bool {
        self.a.is_some() && self.b.is_some()
    }

    /// `Some(a)` when playback has just reached B and must jump back to A.
    pub fn due(&self, pos: f64) -> Option<f64> {
        match (self.a, self.b) {
            (Some(a), Some(b)) if pos >= b && pos < b + ESCAPE_SEC => Some(a),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_cycle_sets_a_then_b_then_clears() {
        let mut l = AbLoop::default();
        l.cycle(10.0);
        assert_eq!((l.a, l.b), (Some(10.0), None));
        assert!(!l.is_active());
        l.cycle(25.5);
        assert_eq!((l.a, l.b), (Some(10.0), Some(25.5)));
        assert!(l.is_active());
        l.cycle(30.0);
        assert_eq!(l, AbLoop::default());
    }

    #[test]
    fn b_too_close_to_a_is_ignored() {
        let mut l = AbLoop::default();
        l.cycle(10.0);
        l.cycle(10.2);
        assert_eq!(l.b, None);
        l.cycle(9.8);
        assert_eq!(l.b, None);
        l.cycle(11.0);
        assert_eq!(l.b, Some(11.0));
    }

    #[test]
    fn a_second_press_before_a_swaps_the_marks() {
        let mut l = AbLoop::default();
        l.cycle(30.0);
        l.cycle(12.0); // seeked back, then pressed
        assert_eq!((l.a, l.b), (Some(12.0), Some(30.0)));
        assert!(l.is_active());
        assert_eq!(l.due(30.0), Some(12.0));
    }

    #[test]
    fn jumps_back_to_a_only_when_b_is_reached_from_inside_the_loop() {
        let mut l = AbLoop::default();
        l.cycle(10.0);
        l.cycle(20.0);
        assert_eq!(l.due(15.0), None);
        assert_eq!(l.due(19.99), None);
        assert_eq!(l.due(20.0), Some(10.0));
        assert_eq!(l.due(20.4), Some(10.0));
        // A deliberate seek well past B leaves the loop alone.
        assert_eq!(l.due(120.0), None);
        // A-only never jumps.
        let mut a_only = AbLoop::default();
        a_only.cycle(5.0);
        assert_eq!(a_only.due(100.0), None);
    }
}
