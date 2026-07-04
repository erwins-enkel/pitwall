use crate::history::History;
use std::collections::HashMap;
use std::time::SystemTime;

/// Scope-qualified identity used to join a runner to its GitHub job. `scope` is
/// either `owner/repo` (repo-scoped) or a bare `owner` (org-scoped); `name` is
/// the GitHub runner name (`runner-N` for docker, the `.runner` agentName for
/// native). Runner names are NOT unique across scopes (e.g. `ltdovr` and
/// `scoop/mensamax-ui` both register as `backontop`), so the scope is required.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunnerKey {
    pub scope: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Docker,
    Native,
}

#[derive(Debug, Clone)]
pub struct RunnerResource {
    pub name: String,
    pub cpu_pct: f64,
    pub mem_bytes: u64,
    pub mem_limit: u64,
    /// `None` when the runner's GitHub identity is unknown (e.g. an unreadable
    /// `.runner`); such rows show resources but never match a job (always idle).
    pub key: Option<RunnerKey>,
    pub kind: SourceKind,
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
/// `Normal` when `limit == 0` (divide-by-zero guard, and the uncapped-native
/// case). Single source of truth for both the per-runner mem cell and the gauge.
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
    pub kind: SourceKind,
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

/// Gauge total = summed memory of the docker/pulse-slice runners only. Native
/// runners live in a different slice (no shared cap) so they don't count here.
pub fn slice_total_bytes(rows: &[RunnerRow]) -> u64 {
    rows.iter()
        .filter(|r| r.kind == SourceKind::Docker)
        .map(|r| r.mem_bytes)
        .sum()
}

fn kind_order(k: SourceKind) -> u8 {
    match k {
        SourceKind::Docker => 0,
        SourceKind::Native => 1,
    }
}

/// Join resources with the jobs map, keyed by scope-qualified [`RunnerKey`].
/// A key present in `jobs` means the runner is busy: `Some(info)` carries the
/// workflow › job detail, `None` is busy-without-detail (org runners). Critical
/// memory escalates the whole row to `NearCap` (overriding Busy); the Warn tier
/// lives on the mem-cell channel via `mem_level`. Rows sort docker-first (by
/// trailing index), then native (by display name). History is keyed by name.
pub fn join(
    resources: Vec<RunnerResource>,
    jobs: &HashMap<RunnerKey, Option<JobInfo>>,
    history: &History,
    warn_ratio: f64,
    crit_ratio: f64,
) -> Vec<RunnerRow> {
    let mut rows: Vec<RunnerRow> = resources
        .into_iter()
        .map(|r| {
            let status = r.key.as_ref().and_then(|k| jobs.get(k));
            let job = status.and_then(|s| s.clone());
            let busy = status.is_some();
            let level = mem_level(r.mem_bytes, r.mem_limit, warn_ratio, crit_ratio);
            // Row color = job state; only Critical escalates to the whole-row
            // NearCap red. The Warn tier lives on the mem-cell channel (see ui).
            let load = if level == MemLevel::Critical {
                Load::NearCap
            } else if busy {
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
                kind: r.kind,
                cpu_hist,
                mem_hist,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        kind_order(a.kind).cmp(&kind_order(b.kind)).then_with(|| {
            // Same kind here; docker orders by trailing index, native by name.
            match a.kind {
                SourceKind::Docker => runner_index(&a.name)
                    .unwrap_or(u32::MAX)
                    .cmp(&runner_index(&b.name).unwrap_or(u32::MAX)),
                SourceKind::Native => a.name.cmp(&b.name),
            }
        })
    });
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

    fn docker_res(name: &str, mem: u64) -> RunnerResource {
        let idx = runner_index(name).unwrap();
        RunnerResource {
            name: name.into(),
            cpu_pct: 1.0,
            mem_bytes: mem,
            mem_limit: CAP,
            key: Some(RunnerKey {
                scope: "erwins-enkel/pulse".into(),
                name: format!("runner-{idx}"),
            }),
            kind: SourceKind::Docker,
        }
    }

    fn native_res(display: &str, scope: &str, agent: &str, mem: u64) -> RunnerResource {
        RunnerResource {
            name: display.into(),
            cpu_pct: 1.0,
            mem_bytes: mem,
            mem_limit: 0, // native cgroups are uncapped
            key: Some(RunnerKey {
                scope: scope.into(),
                name: agent.into(),
            }),
            kind: SourceKind::Native,
        }
    }

    fn pulse_job(idx: u32) -> (RunnerKey, Option<JobInfo>) {
        (
            RunnerKey {
                scope: "erwins-enkel/pulse".into(),
                name: format!("runner-{idx}"),
            },
            Some(JobInfo {
                workflow: "ci".into(),
                job: "test".into(),
                branch: "main".into(),
                started_at: SystemTime::now(),
            }),
        )
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
        // limit == 0 must not divide by zero (uncapped native runners).
        assert_eq!(mem_level(100, 0, WARN, CRIT), MemLevel::Normal);
    }

    #[test]
    fn no_job_is_idle() {
        let rows = join(
            vec![docker_res("pulse-ci-runner-1", 100)],
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
            RunnerKey {
                scope: "erwins-enkel/pulse".into(),
                name: "runner-1".into(),
            },
            Some(JobInfo {
                workflow: "ci".into(),
                job: "test".into(),
                branch: "main".into(),
                started_at: now - Duration::from_secs(30),
            }),
        );
        let rows = join(
            vec![docker_res("pulse-ci-runner-1", 100)],
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
        assert_eq!(rows[0].job.as_ref().unwrap().branch, "main");
    }

    #[test]
    fn busy_in_warn_band_stays_busy_with_warn_mem_level() {
        // A running job at 87% memory: the row stays Busy (green) — the warn
        // tier surfaces only via mem_level, not by hijacking the row color.
        let (k, v) = pulse_job(1);
        let mut jobs = HashMap::new();
        jobs.insert(k, v);
        let rows = join(
            vec![docker_res("pulse-ci-runner-1", (CAP as f64 * 0.87) as u64)],
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
            vec![docker_res("pulse-ci-runner-1", (CAP as f64 * 0.95) as u64)],
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
        let snap = [docker_res("pulse-ci-runner-1", 100)];
        history.record(&snap, &snap);
        history.record(&snap, &snap);
        let rows = join(
            vec![docker_res("pulse-ci-runner-1", 100)],
            &HashMap::new(),
            &history,
            WARN,
            CRIT,
        );
        assert_eq!(rows[0].cpu_hist.len(), 2);
        assert_eq!(rows[0].mem_hist.len(), 2);
        // An unknown runner (no recorded history) gets empty series.
        let empty = join(
            vec![docker_res("pulse-ci-runner-9", 100)],
            &HashMap::new(),
            &history,
            WARN,
            CRIT,
        );
        assert!(empty[0].cpu_hist.is_empty());
    }

    #[test]
    fn rows_sorted_docker_by_index_then_native_by_name_and_gauge_docker_only() {
        let rows = join(
            vec![
                native_res(
                    "scoop-vanscout",
                    "scoop/vanscout",
                    "backontop-vanscout",
                    999,
                ),
                docker_res("pulse-ci-runner-3", 300),
                native_res("ltdovr", "ltdovr", "backontop", 888),
                docker_res("pulse-ci-runner-1", 100),
            ],
            &HashMap::new(),
            &History::default(),
            WARN,
            CRIT,
        );
        // Docker first (by index), then native (by display name).
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "pulse-ci-runner-1",
                "pulse-ci-runner-3",
                "ltdovr",
                "scoop-vanscout"
            ]
        );
        // Gauge sums docker rows only (100 + 300), ignoring native mem.
        assert_eq!(slice_total_bytes(&rows), 400);
    }

    #[test]
    fn scope_qualified_key_resolves_backontop_collision() {
        // Two native runners share agentName `backontop` on different scopes.
        let mut jobs = HashMap::new();
        jobs.insert(
            RunnerKey {
                scope: "scoop/mensamax-ui".into(),
                name: "backontop".into(),
            },
            Some(JobInfo {
                workflow: "ci".into(),
                job: "test".into(),
                branch: "main".into(),
                started_at: SystemTime::now(),
            }),
        );
        let rows = join(
            vec![
                native_res("scoop-mensamax-ui", "scoop/mensamax-ui", "backontop", 10),
                native_res("ltdovr", "ltdovr", "backontop", 10),
            ],
            &jobs,
            &History::default(),
            WARN,
            CRIT,
        );
        // mensamax-ui matches (busy); ltdovr shares the name but not the scope (idle).
        let mensa = rows.iter().find(|r| r.name == "scoop-mensamax-ui").unwrap();
        let ltdovr = rows.iter().find(|r| r.name == "ltdovr").unwrap();
        assert!(matches!(mensa.load, Load::Busy));
        assert!(mensa.job.is_some());
        assert!(matches!(ltdovr.load, Load::Idle));
        assert!(ltdovr.job.is_none());
    }

    #[test]
    fn busy_without_detail_is_busy_but_no_job() {
        // Org runners report busy via the runners endpoint with no workflow›job.
        let mut jobs = HashMap::new();
        jobs.insert(
            RunnerKey {
                scope: "ltdovr".into(),
                name: "backontop".into(),
            },
            None,
        );
        let rows = join(
            vec![native_res("ltdovr", "ltdovr", "backontop", 10)],
            &jobs,
            &History::default(),
            WARN,
            CRIT,
        );
        assert!(matches!(rows[0].load, Load::Busy));
        assert!(rows[0].job.is_none());
    }

    #[test]
    fn resource_only_row_without_key_never_matches() {
        // Unreadable `.runner` → key None → always idle even if a job exists.
        let mut jobs = HashMap::new();
        jobs.insert(
            RunnerKey {
                scope: "scoop/vanscout".into(),
                name: "backontop-vanscout".into(),
            },
            Some(JobInfo {
                workflow: "ci".into(),
                job: "test".into(),
                branch: "main".into(),
                started_at: SystemTime::now(),
            }),
        );
        let row_res = RunnerResource {
            name: "scoop-vanscout".into(),
            cpu_pct: 1.0,
            mem_bytes: 10,
            mem_limit: 0,
            key: None,
            kind: SourceKind::Native,
        };
        let rows = join(vec![row_res], &jobs, &History::default(), WARN, CRIT);
        assert!(matches!(rows[0].load, Load::Idle));
        assert!(rows[0].job.is_none());
    }
}
