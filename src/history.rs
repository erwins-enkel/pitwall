use crate::model::RunnerResource;
use std::collections::HashMap;

/// Number of samples retained per runner series. At the 2s resource poll
/// cadence this is ~40s of history.
pub const WINDOW: usize = 20;

#[derive(Default)]
struct RunnerSeries {
    cpu: Vec<f64>,      // percent, oldest→newest
    mem_frac: Vec<f64>, // fraction 0..1 of the mem limit, oldest→newest
}

/// Bounded per-runner history of CPU% and memory fill, keyed by container name
/// (`pulse-ci-runner-N`, stable across ephemeral re-registration).
#[derive(Default)]
pub struct History {
    series: HashMap<String, RunnerSeries>,
}

fn push_capped(v: &mut Vec<f64>, x: f64) {
    v.push(x);
    if v.len() > WINDOW {
        v.remove(0); // FIFO eviction; WINDOW is tiny so O(n) is fine
    }
}

impl History {
    /// Appends one sample per runner in the snapshot and prunes series for
    /// runners absent from it (deregistered ephemeral runners). Guards a zero
    /// `mem_limit` (resource.rs sets `unwrap_or(0)`) to avoid divide-by-zero.
    pub fn record(&mut self, resources: &[RunnerResource]) {
        for r in resources {
            let s = self.series.entry(r.name.clone()).or_default();
            push_capped(&mut s.cpu, r.cpu_pct);
            let frac = if r.mem_limit > 0 {
                r.mem_bytes as f64 / r.mem_limit as f64
            } else {
                0.0
            };
            push_capped(&mut s.mem_frac, frac);
        }
        self.series
            .retain(|name, _| resources.iter().any(|r| &r.name == name));
    }

    /// CPU% series for a runner, oldest→newest; empty if unknown.
    pub fn cpu(&self, name: &str) -> &[f64] {
        self.series.get(name).map_or(&[], |s| s.cpu.as_slice())
    }

    /// Memory-fill fraction series (0..1) for a runner, oldest→newest; empty if
    /// unknown.
    pub fn mem_frac(&self, name: &str) -> &[f64] {
        self.series.get(name).map_or(&[], |s| s.mem_frac.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(name: &str, cpu: f64, mem: u64, limit: u64) -> RunnerResource {
        RunnerResource {
            name: name.into(),
            cpu_pct: cpu,
            mem_bytes: mem,
            mem_limit: limit,
        }
    }

    #[test]
    fn records_one_point_per_runner_per_snapshot() {
        let mut h = History::default();
        h.record(&[res("r-1", 10.0, 100, 1000), res("r-2", 20.0, 200, 1000)]);
        h.record(&[res("r-1", 12.0, 150, 1000), res("r-2", 22.0, 250, 1000)]);
        assert_eq!(h.cpu("r-1"), &[10.0, 12.0]);
        assert_eq!(h.cpu("r-2"), &[20.0, 22.0]);
        assert_eq!(h.mem_frac("r-1"), &[0.1, 0.15]);
    }

    #[test]
    fn bounded_to_window_with_fifo_eviction() {
        let mut h = History::default();
        for i in 0..(WINDOW as u64 + 5) {
            h.record(&[res("r-1", i as f64, 0, 1000)]);
        }
        let cpu = h.cpu("r-1");
        assert_eq!(cpu.len(), WINDOW);
        // Oldest 5 evicted: series starts at 5, ends at WINDOW+4.
        assert_eq!(cpu.first(), Some(&5.0));
        assert_eq!(cpu.last(), Some(&((WINDOW as f64) + 4.0)));
    }

    #[test]
    fn prunes_absent_runners() {
        let mut h = History::default();
        h.record(&[res("r-1", 1.0, 0, 1000), res("r-2", 2.0, 0, 1000)]);
        h.record(&[res("r-1", 3.0, 0, 1000)]); // r-2 gone
        assert_eq!(h.cpu("r-1"), &[1.0, 3.0]);
        assert!(h.cpu("r-2").is_empty());
    }

    #[test]
    fn zero_mem_limit_yields_zero_fraction() {
        let mut h = History::default();
        h.record(&[res("r-1", 0.0, 500, 0)]);
        assert_eq!(h.mem_frac("r-1"), &[0.0]);
    }

    #[test]
    fn unknown_runner_returns_empty() {
        let h = History::default();
        assert!(h.cpu("nope").is_empty());
        assert!(h.mem_frac("nope").is_empty());
    }
}
