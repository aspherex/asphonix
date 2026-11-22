//! Property tests of the A-B loop state machine (`audio::abloop::AbLoop`): any sequence of presses at any positions
//! (negative and past-the-end included) keeps the marks ordered and far enough apart, and the jump back only fires
//! from just past B.
use proptest::prelude::*;
use sonix_core::audio::abloop::{AbLoop, MIN_LOOP_SEC};

proptest! {
    #[test]
    fn marks_stay_ordered_and_apart_under_any_presses(presses in prop::collection::vec(-50f64..5000., 0..40)) {
        let mut l = AbLoop::default();
        for (n, p) in presses.iter().enumerate() {
            let before = l;
            l.cycle(*p);
            if let Some(a) = l.a {
                prop_assert!(a >= 0.0, "A never negative (press {} at {})", n, p);
            }
            if let Some(b) = l.b {
                let a = l.a.expect("B only with A");
                prop_assert!(b >= a + MIN_LOOP_SEC, "B {} too close to A {}", b, a);
            }
            // the cycle: empty -> A -> A+B -> empty; a press too close to A waits (no change)
            match (before.a, before.b) {
                (None, _) => prop_assert!(l.a.is_some() && l.b.is_none()),
                (Some(_), None) => prop_assert!(l.b.is_some() || l == before),
                (Some(_), Some(_)) => prop_assert_eq!(l, AbLoop::default()),
            }
            prop_assert_eq!(l.is_active(), l.a.is_some() && l.b.is_some());
        }
    }

    #[test]
    fn the_jump_back_fires_only_just_past_b_and_lands_on_a(a in 0f64..600., len in MIN_LOOP_SEC..300., pos in -10f64..1200.) {
        let mut l = AbLoop::default();
        l.cycle(a);
        prop_assert_eq!(l.due(pos), None, "A alone never jumps");
        l.cycle(a + len);
        let b = l.b.expect("B set");
        match l.due(pos) {
            Some(to) => {
                prop_assert_eq!(Some(to), l.a);
                prop_assert!(pos >= b && pos - b < 1.5, "fired at {} for B {}", pos, b);
            }
            None => prop_assert!(pos < b || pos >= b + 1.5),
        }
    }
}
