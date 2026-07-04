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

#[derive(Debug, Clone)]
pub struct RunnerRow {
    pub name: String,
    pub cpu_pct: f64,
    pub mem_bytes: u64,
    pub mem_limit: u64,
    pub job: Option<JobInfo>,
    pub load: Load,
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
) -> Vec<RunnerRow> {
    let mut rows: Vec<RunnerRow> = resources
        .into_iter()
        .map(|r| {
            let job = runner_index(&r.name).and_then(|i| jobs.get(&i)).cloned();
            let near_cap = r.mem_limit > 0 && (r.mem_bytes as f64 / r.mem_limit as f64) >= 0.9;
            let load = if near_cap {
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

    fn res(name: &str, mem: u64) -> RunnerResource {
        RunnerResource {
            name: name.into(),
            cpu_pct: 1.0,
            mem_bytes: mem,
            mem_limit: 8 * 1024 * 1024 * 1024,
        }
    }

    #[test]
    fn parses_runner_index() {
        assert_eq!(runner_index("ci-runner-4"), Some(4));
        assert_eq!(runner_index("runner-2"), Some(2));
        assert_eq!(runner_index("nope"), None);
    }

    #[test]
    fn no_job_is_idle() {
        let rows = join(
            vec![res("ci-runner-1", 100)],
            &HashMap::new(),
            &History::default(),
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
        let rows = join(vec![res("ci-runner-1", 100)], &jobs, &History::default());
        assert!(matches!(rows[0].load, Load::Busy));
        assert_eq!(
            elapsed_secs(rows[0].job.as_ref().unwrap().started_at, now),
            30
        );
    }

    #[test]
    fn high_mem_is_near_cap() {
        let cap = 8u64 * 1024 * 1024 * 1024;
        let rows = join(
            vec![res("ci-runner-1", (cap as f64 * 0.95) as u64)],
            &HashMap::new(),
            &History::default(),
        );
        assert!(matches!(rows[0].load, Load::NearCap));
    }

    #[test]
    fn join_copies_history_into_rows() {
        let mut history = History::default();
        history.record(&[res("ci-runner-1", 100)]);
        history.record(&[res("ci-runner-1", 100)]);
        let rows = join(vec![res("ci-runner-1", 100)], &HashMap::new(), &history);
        assert_eq!(rows[0].cpu_hist.len(), 2);
        assert_eq!(rows[0].mem_hist.len(), 2);
        // An unknown runner (no recorded history) gets empty series.
        let empty = join(vec![res("ci-runner-9", 100)], &HashMap::new(), &history);
        assert!(empty[0].cpu_hist.is_empty());
    }

    #[test]
    fn rows_sorted_by_index_and_slice_summed() {
        let rows = join(
            vec![res("ci-runner-3", 300), res("ci-runner-1", 100)],
            &HashMap::new(),
            &History::default(),
        );
        assert_eq!(rows[0].name, "ci-runner-1");
        assert_eq!(slice_total_bytes(&rows), 400);
    }
}
