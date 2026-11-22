//! The decode -> render handoff: a lock-free SPSC byte ring buffer
//! (`rtrb`) sized to ~1-2s of PCM at the negotiated output format.
//!
//! The decode thread is the sole producer, the render thread the sole
//! consumer. The render thread never allocates or locks: it only calls
//! `Consumer::read_chunk`/`Chunk::as_slices` and copies bytes into the
//! device buffer.

use rtrb::{Consumer, Producer, RingBuffer};

/// Target amount of buffered PCM (memory budget:
/// "decoded PCM only in a ring buffer of ~1-2s").
pub const TARGET_BUFFER_SECONDS: f64 = 1.5;

/// Creates a new PCM ring buffer sized for `bytes_per_frame` at `sample_rate`,
/// holding `TARGET_BUFFER_SECONDS` of audio.
pub fn new_pcm_ring(sample_rate: u32, bytes_per_frame: usize) -> (Producer<u8>, Consumer<u8>) {
    let capacity =
        ((sample_rate as f64 * TARGET_BUFFER_SECONDS) as usize * bytes_per_frame).max(bytes_per_frame * 64);
    RingBuffer::<u8>::new(capacity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_bytes_in_order() {
        let (mut prod, mut cons) = new_pcm_ring(44_100, 4);
        let data: Vec<u8> = (0..200u8).collect();
        let written = prod.write_chunk_uninit(data.len()).unwrap().fill_from_iter(data.iter().copied());
        assert_eq!(written, data.len());

        let mut out = vec![0u8; data.len()];
        let chunk = cons.read_chunk(data.len()).unwrap();
        let (a, b) = chunk.as_slices();
        out[..a.len()].copy_from_slice(a);
        out[a.len()..].copy_from_slice(b);
        chunk.commit_all();
        assert_eq!(out, data);
    }

    #[test]
    fn capacity_is_sized_for_target_buffer() {
        let (prod, _cons) = new_pcm_ring(96_000, 8);
        // ~1.5s @ 96kHz, 8 bytes/frame.
        assert!(prod.buffer().capacity() >= (96_000.0 * TARGET_BUFFER_SECONDS) as usize * 8);
    }

    #[test]
    fn small_rates_still_get_a_usable_minimum() {
        let (prod, _cons) = new_pcm_ring(8_000, 2);
        assert!(prod.buffer().capacity() >= 64 * 2);
    }
}
