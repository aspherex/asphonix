//! Measurement support for `scripts/perf.ps1`: frame-interval statistics (time between
//! consecutive root `render()` calls) and the scripted timeline (`ASPHONIX_SCRIPT=1`):
//! idle, an end-to-end scroll of the Tracks list, Now Playing, then back to Tracks.
//! The harness reads the `phase ...` lines from `$ASPHONIX_LOG`.
use std::time::Instant;

#[derive(Default)]
pub struct FrameStats {
    last: Option<Instant>,
    pub intervals_ms: Vec<f32>,
    pub enabled: bool,
}

impl FrameStats {
    pub fn new(enabled: bool) -> Self {
        Self { enabled, ..Default::default() }
    }
    pub fn tick(&mut self) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if let Some(l) = self.last {
            self.intervals_ms.push(now.duration_since(l).as_secs_f32() * 1000.0);
        }
        self.last = Some(now);
    }
    pub fn reset(&mut self) {
        self.intervals_ms.clear();
        self.last = None;
    }
    /// Idle gaps (nothing dirty, so no render) end an interval series: forget the anchor.
    pub fn summary(&self) -> Option<Summary> {
        summarize(&self.intervals_ms)
    }
}

#[derive(Debug, PartialEq)]
pub struct Summary {
    pub frames: usize,
    pub p50: f32,
    pub p95: f32,
    pub p99: f32,
    pub max: f32,
    pub over_16_7: usize,
}

pub fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let ix = ((sorted.len() as f32 - 1.0) * p).round() as usize;
    sorted[ix.min(sorted.len() - 1)]
}

pub fn summarize(v: &[f32]) -> Option<Summary> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    Some(Summary {
        frames: s.len(),
        p50: percentile(&s, 0.50),
        p95: percentile(&s, 0.95),
        p99: percentile(&s, 0.99),
        max: *s.last().unwrap(),
        over_16_7: s.iter().filter(|x| **x > 16.7).count(),
    })
}

impl Summary {
    pub fn line(&self, name: &str) -> String {
        format!(
            "frames {name} n={} p50={:.1} p95={:.1} p99={:.1} max={:.1} over16.7={}",
            self.frames, self.p50, self.p95, self.p99, self.max, self.over_16_7
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_and_overruns() {
        let v: Vec<f32> = (1..=100).map(|x| x as f32).collect();
        let s = summarize(&v).unwrap();
        assert_eq!(s.frames, 100);
        assert_eq!(s.p50, 51.0);
        assert_eq!(s.p99, 99.0);
        assert_eq!(s.max, 100.0);
        assert_eq!(s.over_16_7, 84);
        assert_eq!(summarize(&[]), None);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }

    #[test]
    fn disabled_stats_record_nothing() {
        let mut f = FrameStats::new(false);
        f.tick();
        f.tick();
        assert!(f.intervals_ms.is_empty());
        let mut g = FrameStats::new(true);
        g.tick();
        g.tick();
        assert_eq!(g.intervals_ms.len(), 1);
        g.reset();
        assert!(g.summary().is_none());
    }
}
