//! Flow sequencing is reproducible per seed (the seeded round shuffle is a test-only hook, hence an in-crate module;
//! the seed-independent properties live in `tests/queue_props.rs`).
use super::*;
use proptest::prelude::*;

fn round(seed: u64, arc: FlowArc, pool: &[String], features: &HashMap<String, FlowTrack>) -> Vec<String> {
    let mut q = Queue::new();
    q.set_window_size(MIN_WINDOW_SIZE);
    q.set_source_flow_seeded(arc, pool.to_vec(), features.clone(), seed);
    let mut out: Vec<String> = q.current_id().map(str::to_string).into_iter().collect();
    while let Some(id) = q.next() {
        out.push(id.to_string());
    }
    out
}

proptest! {
    #[test]
    fn a_flow_round_is_the_same_for_the_same_seed(
        bpms in prop::collection::vec(prop::option::of(60f32..180.), 1..80),
        seed in any::<u64>(),
        arc in prop_oneof![Just(FlowArc::Steady), Just(FlowArc::Build), Just(FlowArc::Peak)],
    ) {
        let pool: Vec<String> = (0..bpms.len()).map(|i| format!("p{i}")).collect();
        let features: HashMap<String, FlowTrack> = pool.iter().cloned().zip(bpms.iter().map(|b| FlowTrack { bpm: *b, ..FlowTrack::default() })).collect();
        let a = round(seed, arc, &pool, &features);
        prop_assert_eq!(&a, &round(seed, arc, &pool, &features));
        let mut sorted = a.clone();
        sorted.sort();
        let mut want = pool.clone();
        want.sort();
        prop_assert_eq!(sorted, want, "a permutation of the pool");
    }
}
