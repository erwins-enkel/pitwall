use crate::history::History;
use std::collections::HashMap;
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct RunnerResource {
    pub name: String,
    pub cpu_pct: f64,
    pub mem_bytes: u64,
    pub mem_limit: u64,
}

#[derive(Debug, Clone)]
pub struct JobInfo {
    pub workflow: String,
    pub job: String,
    pub branch: String,
    pub started_at: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    Idle,
    Busy,
    NearCap,
}

/// Memory-pressure tier for a single ratio (a runner's `used/limit` or the
/// slice's `total/cap`). Rendered on its own channel — the `mem` cell / gauge —
/// separately from `Load`, so a warn-band busy runner keeps its green row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemLevel {
    Normal,
    Warn,
    Critical,
}

/// Classify a memory ratio against the warn/critical thresholds. Returns
/// `Normal` when `limit == 0` (divide-by-zero guard). Single source of truth for
/// both the per-runner mem cell and the slice gauge.
pub fn mem_level(used: u64, limit: u64, warn_ratio: f64, crit_ratio: f64) -> MemLevel {
    if limit == 0 {
        return MemLevel::Normal;
    }
    let ratio = used as f64 / limit as f64;
    if ratio >= crit_ratio {
        MemLevel::Critical
    } else if ratio >= warn_ratio {
        MemLevel::Warn
    } else {
        MemLevel::Normal
    }
}

#[derive(Debug, Clone)]
pub struct RunnerRow {
    pub name: String,
    pub cpu_pct: f64,
    pub mem_bytes: u64,
    pub mem_limit: u64,
    pub job: Option<JobInfo>,
    pub load: Load,
    pub mem_level: MemLevel,
    pub cpu_hist: Vec<f64>, // percent, oldest→newest
    pub mem_hist: Vec<f64>, // fraction 0..1, oldest→newest
}

pub fn runner_index(name: &str) -> Option<u32> {
    name.rsplit('-').next()?.parse().ok()
}

pub fn elapsed_secs(started: SystemTime, now: SystemTime) -> u64 {
    now.duration_since(started)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn slice_total_bytes(rows: &[RunnerRow]) -> u64 {
    rows.iter().map(|r| r.mem_bytes).sum()
}

pub fn join(
    resources: Vec<RunnerResource>,
    jobs: &HashMap<u32, JobInfo>,
    history: &History,
    warn_ratio: f64,
    crit_ratio: f64,
) -> Vec<RunnerRow> {
    let mut rows: Vec<RunnerRow> = resources
        .into_iter()
        .map(|r| {
            let job = runner_index(&r.name).and_then(|i| jobs.get(&i)).cloned();
            let level = mem_level(r.mem_bytes, r.mem_limit, warn_ratio, crit_ratio);
            // Row color = job state; only Critical escalates to the whole-row
            // NearCap red. The Warn tier lives on the mem-cell channel (see ui).
            let load = if level == MemLevel::Critical {
                Load::NearCap
            } else if job.is_some() {
                Load::Busy
            } else {
                Load::Idle
            };
            let cpu_hist = history.cpu(&r.name).to_vec();
            let mem_hist = history.mem_frac(&r.name).to_vec();
            RunnerRow {
                name: r.name,
                cpu_pct: r.cpu_pct,
                mem_bytes: r.mem_bytes,
                mem_limit: r.mem_limit,
                job,
                load,
                mem_level: level,
                cpu_hist,
                mem_hist,
            }
        })
        .collect();
    rows.sort_by_key(|r| runner_index(&r.name).unwrap_or(u32::MAX));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    const CAP: u64 = 8 * 1024 * 1024 * 1024;
    // Default thresholds mirror config: warn 85%, crit 90%.
    const WARN: f64 = 0.85;
    const CRIT: f64 = 0.90;

    fn res(name: &str, mem: u64) -> RunnerResource {
        RunnerResource {
            name: name.into(),
            cpu_pct: 1.0,
            mem_bytes: mem,
            mem_limit: CAP,
        }
    }

    #[test]
    fn parses_runner_index() {
        assert_eq!(runner_index("ci-runner-4"), Some(4));
        assert_eq!(runner_index("runner-2"), Some(2));
        assert_eq!(runner_index("nope"), None);
    }

    #[test]
    fn mem_level_bands() {
        // Use limit == 100 so `used` is an exact percentage — lets us assert the
        // inclusive `>=` boundaries without integer-truncation fuzz.
        assert_eq!(mem_level(0, 100, WARN, CRIT), MemLevel::Normal);
        assert_eq!(mem_level(80, 100, WARN, CRIT), MemLevel::Normal);
        assert_eq!(mem_level(84, 100, WARN, CRIT), MemLevel::Normal);
        // At exactly the warn threshold → Warn (inclusive).
        assert_eq!(mem_level(85, 100, WARN, CRIT), MemLevel::Warn);
        assert_eq!(mem_level(89, 100, WARN, CRIT), MemLevel::Warn);
        // At exactly the crit threshold → Critical (inclusive).
        assert_eq!(mem_level(90, 100, WARN, CRIT), MemLevel::Critical);
        assert_eq!(mem_level(95, 100, WARN, CRIT), MemLevel::Critical);
        // limit == 0 must not divide by zero.
        assert_eq!(mem_level(100, 0, WARN, CRIT), MemLevel::Normal);
    }

    #[test]
    fn no_job_is_idle() {
        let rows = join(
            vec![res("ci-runner-1", 100)],
            &HashMap::new(),
            &History::default(),
            WARN,
            CRIT,
        );
        assert!(matches!(rows[0].load, Load::Idle));
        assert!(rows[0].job.is_none());
    }

    #[test]
    fn job_present_is_busy() {
        let now = SystemTime::now();
        let mut jobs = HashMap::new();
        jobs.insert(
            1u32,
            JobInfo {
                workflow: "ci".into(),
                job: "test".into(),
                branch: "main".into(),
                started_at: now - Duration::from_secs(30),
            },
        );
        let rows = join(
            vec![res("ci-runner-1", 100)],
            &jobs,
            &History::default(),
            WARN,
            CRIT,
        );
        assert!(matches!(rows[0].load, Load::Busy));
        assert_eq!(
            elapsed_secs(rows[0].job.as_ref().unwrap().started_at, now),
            30
        );
    }

    #[test]
    fn busy_in_warn_band_stays_busy_with_warn_mem_level() {
        // A running job at 87% memory: the row stays Busy (green) — the warn
        // tier surfaces only via mem_level, not by hijacking the row color.
        let now = SystemTime::now();
        let mut jobs = HashMap::new();
        jobs.insert(
            1u32,
            JobInfo {
                workflow: "ci".into(),
                job: "test".into(),
                branch: "main".into(),
                started_at: now,
            },
        );
        let rows = join(
            vec![res("ci-runner-1", (CAP as f64 * 0.87) as u64)],
            &jobs,
            &History::default(),
            WARN,
            CRIT,
        );
        assert!(matches!(rows[0].load, Load::Busy));
        assert_eq!(rows[0].mem_level, MemLevel::Warn);
    }

    #[test]
    fn high_mem_is_near_cap() {
        let rows = join(
            vec![res("ci-runner-1", (CAP as f64 * 0.95) as u64)],
            &HashMap::new(),
            &History::default(),
            WARN,
            CRIT,
        );
        assert!(matches!(rows[0].load, Load::NearCap));
        assert_eq!(rows[0].mem_level, MemLevel::Critical);
    }

    #[test]
    fn join_copies_history_into_rows() {
        let mut history = History::default();
        history.record(&[res("ci-runner-1", 100)]);
        history.record(&[res("ci-runner-1", 100)]);
        let rows = join(
            vec![res("ci-runner-1", 100)],
            &HashMap::new(),
            &history,
            WARN,
            CRIT,
        );
        assert_eq!(rows[0].cpu_hist.len(), 2);
        assert_eq!(rows[0].mem_hist.len(), 2);
        // An unknown runner (no recorded history) gets empty series.
        let empty = join(
            vec![res("ci-runner-9", 100)],
            &HashMap::new(),
            &history,
            WARN,
            CRIT,
        );
        assert!(empty[0].cpu_hist.is_empty());
    }

    #[test]
    fn rows_sorted_by_index_and_slice_summed() {
        let rows = join(
            vec![res("ci-runner-3", 300), res("ci-runner-1", 100)],
            &HashMap::new(),
            &History::default(),
            WARN,
            CRIT,
        );
        assert_eq!(rows[0].name, "ci-runner-1");
        assert_eq!(slice_total_bytes(&rows), 400);
    }
}
