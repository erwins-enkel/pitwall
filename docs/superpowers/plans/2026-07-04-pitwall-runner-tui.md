# pitwall Runner Stats TUI — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A btop-like Rust TUI showing, per docker CI runner, live CPU/mem joined with the workflow/job currently running.

**Architecture:** Async tokio binary. Two background sources (bollard docker-stats stream; `gh` shell-out for jobs) push updates through an mpsc event channel into a shared `AppState`; a ratatui render loop reads it and draws a table + slice gauge. Pure `model` + math modules hold all testable logic.

**Tech Stack:** Rust 1.95, tokio, ratatui 0.30 + crossterm, bollard 0.20, futures-util, serde/serde_json, anyhow.

## Global Constraints

- Rust edition 2021; toolchain already installed (rustc 1.95).
- Docker access is **rootless**: socket `unix:///run/user/1000/docker.sock` (respect `DOCKER_HOST` env if set). The rootful `/var/run/docker.sock` does NOT see these containers.
- **Join key (validated against live `gh api`):** container `ci-runner-N` ↔ GitHub `runner_name` = `runner-N`. Match jobs whose `runner_name` is **exactly `runner-<digits>`**, then key on the integer N.
- **Exclude non-self-hosted jobs:** `runner_name` may be a GitHub-hosted runner like `"GitHub Actions 1000013810"` (confirmed in real payloads) — these must NOT join. The strict `^runner-\d+$` match handles this.
- **Only `status == "in_progress"` jobs carry a live runner.** Queued jobs have no/blank `runner_name`; **completed jobs retain a stale `runner_name`** and must be excluded. Filter runs with `?status=in_progress` (bounds API calls/rate limits) AND filter jobs to `in_progress`.
- Container up + no in-progress GitHub job = **idle** (expected steady state, never an error).
- Jobs polled every 15s (rate-limit safe); docker stats polled every 2s.
- **All environment coupling is config, loaded at startup** (`src/config.rs`), overridable by env with defaults: `PITWALL_SOCKET` (default `$DOCKER_HOST` sans `unix://`, else `/run/user/$UID/docker.sock`), `PITWALL_REPO` = `owner/repo`, `PITWALL_PREFIX` = `ci-runner-`, `PITWALL_SLICE_CAP_GIB` = `24`. No value is hardcoded at a use-site; sources/UI read `Config`.
- **Never panic into a broken terminal:** `ratatui::init()` installs a panic hook that restores; sources degrade (socket down / `gh` unauthenticated / zero runners) to a status banner + empty state, never a process exit.
- `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` must all pass; never suppress warnings to pass.
- Binary installs to `~/.local/bin/pitwall`.

---

### Task 1: Project scaffold + lint gate + config module

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `src/config.rs`, `rustfmt.toml`, `.githooks/pre-commit`
- Modify: `.gitignore`

**Interfaces:**
- Produces:
  - a buildable binary crate named `pitwall` with all deps declared; a committed pre-commit hook running fmt+clippy+test.
  - `pub struct Config { pub socket_path: String, pub repo: String, pub prefix: String, pub slice_cap_bytes: u64 }`
  - `pub fn Config::from_env() -> Config` — reads `PITWALL_SOCKET` (else `$DOCKER_HOST` sans `unix://`, else `/run/user/$UID/docker.sock`), `PITWALL_REPO`, `PITWALL_PREFIX`, `PITWALL_SLICE_CAP_GIB` (→ bytes), applying the documented defaults. Consumed by `resource`, `jobs`, and `ui`.

- [ ] **Step 1: Init crate** — `cargo init --name pitwall .` (repo already exists; this adds `Cargo.toml` + `src/main.rs`). If `cargo init` refuses on a non-empty dir, create `Cargo.toml` and `src/main.rs` by hand.

- [ ] **Step 2: Write `Cargo.toml` dependencies**

```toml
[package]
name = "pitwall"
version = "0.1.0"
edition = "2021"

[dependencies]
anyhow = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "process"] }
futures-util = "0.3"
ratatui = "0.30"
crossterm = { version = "0.29", features = ["event-stream"] }
bollard = "0.20"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
humantime = "2"

[profile.release]
strip = true
lto = true
```

- [ ] **Step 3: Minimal `src/main.rs`**

```rust
fn main() {
    println!("pitwall");
}
```

- [ ] **Step 4: `.gitignore` add** — ensure a line `/target` exists (append if missing).

- [ ] **Step 5: `rustfmt.toml`**

```toml
edition = "2021"
```

- [ ] **Step 6: Pre-commit hook** — `.githooks/pre-commit` (chmod +x):

```bash
#!/usr/bin/env bash
set -euo pipefail
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --quiet
```

Then `git config core.hooksPath .githooks`.

- [ ] **Step 7: `src/config.rs` with a defaults test (TDD)** — add `mod config;` to `main.rs`.

```rust
#[derive(Clone)]
pub struct Config {
    pub socket_path: String,
    pub repo: String,
    pub prefix: String,
    pub slice_cap_bytes: u64,
}

impl Config {
    pub fn from_env() -> Config {
        let socket_path = std::env::var("PITWALL_SOCKET").ok().unwrap_or_else(|| {
            std::env::var("DOCKER_HOST")
                .ok()
                .map(|h| h.trim_start_matches("unix://").to_string())
                .unwrap_or_else(|| {
                    let uid = std::env::var("UID").ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1000);
                    format!("/run/user/{uid}/docker.sock")
                })
        });
        let repo = std::env::var("PITWALL_REPO").unwrap_or_else(|_| "owner/repo".into());
        let prefix = std::env::var("PITWALL_PREFIX").unwrap_or_else(|_| "ci-runner-".into());
        let cap_gib = std::env::var("PITWALL_SLICE_CAP_GIB").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(24);
        Config { socket_path, repo, prefix, slice_cap_bytes: cap_gib * 1024 * 1024 * 1024 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cap_gib_converts_to_bytes_default_24() {
        // Defaults hold when env is unset in the test process.
        std::env::remove_var("PITWALL_SLICE_CAP_GIB");
        std::env::remove_var("PITWALL_REPO");
        let c = Config::from_env();
        assert_eq!(c.slice_cap_bytes, 24 * 1024 * 1024 * 1024);
        assert_eq!(c.repo, "owner/repo");
        assert_eq!(c.prefix, "ci-runner-");
    }
}
```

- [ ] **Step 8: Verify** — `cargo build && cargo test config && cargo clippy --all-targets -- -D warnings && cargo fmt --check`. Expected: clean build, config test passes, no warnings.

- [ ] **Step 9: Commit** — `git add -A && git commit -m "chore: scaffold pitwall rust crate + lint hook + config"`

---

### Task 2: `model` — types and the join (pure, TDD)

**Files:**
- Create: `src/model.rs`
- Modify: `src/main.rs` (add `mod model;`)

**Interfaces:**
- Produces:
  - `pub struct RunnerResource { pub name: String, pub cpu_pct: f64, pub mem_bytes: u64, pub mem_limit: u64 }`
  - `pub struct JobInfo { pub workflow: String, pub job: String, pub started_at: SystemTime }`
  - `pub enum Load { Idle, Busy, NearCap }`
  - `pub struct RunnerRow { pub name: String, pub cpu_pct: f64, pub mem_bytes: u64, pub mem_limit: u64, pub job: Option<JobInfo>, pub load: Load }`
  - `pub fn join(resources: Vec<RunnerResource>, jobs: &HashMap<u32, JobInfo>, now: SystemTime) -> Vec<RunnerRow>` — sorts by runner index; `load` = `Idle` when no job, `NearCap` when `mem_bytes as f64 / mem_limit as f64 >= 0.9`, else `Busy`.
  - `pub fn runner_index(name: &str) -> Option<u32>` — parses trailing integer from `ci-runner-4` → `4`.
  - `pub fn slice_total_bytes(rows: &[RunnerRow]) -> u64` — sums `mem_bytes`.
  - `pub fn elapsed_secs(started: SystemTime, now: SystemTime) -> u64`.

- [ ] **Step 1: Failing tests** — append to `src/model.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn res(name: &str, mem: u64) -> RunnerResource {
        RunnerResource { name: name.into(), cpu_pct: 1.0, mem_bytes: mem, mem_limit: 8 * 1024 * 1024 * 1024 }
    }

    #[test]
    fn parses_runner_index() {
        assert_eq!(runner_index("ci-runner-4"), Some(4));
        assert_eq!(runner_index("runner-2"), Some(2));
        assert_eq!(runner_index("nope"), None);
    }

    #[test]
    fn no_job_is_idle() {
        let rows = join(vec![res("ci-runner-1", 100)], &HashMap::new(), SystemTime::now());
        assert!(matches!(rows[0].load, Load::Idle));
        assert!(rows[0].job.is_none());
    }

    #[test]
    fn job_present_is_busy() {
        let now = SystemTime::now();
        let mut jobs = HashMap::new();
        jobs.insert(1u32, JobInfo { workflow: "ci".into(), job: "test".into(), started_at: now - Duration::from_secs(30) });
        let rows = join(vec![res("ci-runner-1", 100)], &jobs, now);
        assert!(matches!(rows[0].load, Load::Busy));
        assert_eq!(elapsed_secs(rows[0].job.as_ref().unwrap().started_at, now), 30);
    }

    #[test]
    fn high_mem_is_near_cap() {
        let cap = 8u64 * 1024 * 1024 * 1024;
        let rows = join(vec![res("ci-runner-1", (cap as f64 * 0.95) as u64)], &HashMap::new(), SystemTime::now());
        assert!(matches!(rows[0].load, Load::NearCap));
    }

    #[test]
    fn rows_sorted_by_index_and_slice_summed() {
        let rows = join(vec![res("ci-runner-3", 300), res("ci-runner-1", 100)], &HashMap::new(), SystemTime::now());
        assert_eq!(rows[0].name, "ci-runner-1");
        assert_eq!(slice_total_bytes(&rows), 400);
    }
}
```

- [ ] **Step 2: Run — expect fail** — `cargo test model` → compile error / fail.

- [ ] **Step 3: Implement `src/model.rs`** (above the test module):

```rust
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
}

pub fn runner_index(name: &str) -> Option<u32> {
    name.rsplit('-').next()?.parse().ok()
}

pub fn elapsed_secs(started: SystemTime, now: SystemTime) -> u64 {
    now.duration_since(started).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn slice_total_bytes(rows: &[RunnerRow]) -> u64 {
    rows.iter().map(|r| r.mem_bytes).sum()
}

pub fn join(
    resources: Vec<RunnerResource>,
    jobs: &HashMap<u32, JobInfo>,
    _now: SystemTime,
) -> Vec<RunnerRow> {
    let mut rows: Vec<RunnerRow> = resources
        .into_iter()
        .map(|r| {
            let job = runner_index(&r.name).and_then(|i| jobs.get(&i)).cloned();
            let near_cap = r.mem_limit > 0 && (r.mem_bytes as f64 / r.mem_limit as f64) >= 0.9;
            let load = match (&job, near_cap) {
                (None, _) => Load::Idle,
                (Some(_), true) => Load::NearCap,
                (Some(_), false) => Load::Busy,
            };
            RunnerRow {
                name: r.name,
                cpu_pct: r.cpu_pct,
                mem_bytes: r.mem_bytes,
                mem_limit: r.mem_limit,
                job,
                load,
            }
        })
        .collect();
    rows.sort_by_key(|r| runner_index(&r.name).unwrap_or(u32::MAX));
    rows
}
```

Note: `NearCap` in the test with no job asserts NearCap — adjust `load` rule so mem≥90% wins regardless of job: change match to check `near_cap` first: `if near_cap { NearCap } else if job.is_some() { Busy } else { Idle }`. Use THIS rule (the `high_mem_is_near_cap` test has no job).

- [ ] **Step 4: Run — expect pass** — `cargo test model`. Fix the `load` rule per the note until all 5 pass.

- [ ] **Step 5: Commit** — `git commit -am "feat: runner model + join logic"`

---

### Task 3: CPU%/mem math (pure, TDD)

**Files:**
- Create: `src/stats_math.rs`
- Modify: `src/main.rs` (`mod stats_math;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub fn cpu_pct(cpu_total: u64, precpu_total: u64, system: u64, presystem: u64, online: u64) -> f64` — docker's formula: `((cpu_total-precpu_total) / (system-presystem)) * online * 100`, guarding zero system delta → 0.0.
  - `pub fn mem_used(usage: u64, inactive_file: u64) -> u64` — `usage.saturating_sub(inactive_file)` (matches docker CLI).

- [ ] **Step 1: Failing tests** — `src/stats_math.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_percentage_matches_docker_formula() {
        // 1 full core busy over the interval on a 4-core box.
        let pct = cpu_pct(2_000_000_000, 1_000_000_000, 8_000_000_000, 4_000_000_000, 4);
        assert!((pct - 100.0).abs() < 0.001, "got {pct}");
    }

    #[test]
    fn cpu_zero_system_delta_is_zero() {
        assert_eq!(cpu_pct(10, 5, 100, 100, 4), 0.0);
    }

    #[test]
    fn mem_subtracts_inactive_file() {
        assert_eq!(mem_used(1000, 400), 600);
        assert_eq!(mem_used(300, 400), 0);
    }
}
```

- [ ] **Step 2: Run — expect fail** — `cargo test stats_math`.

- [ ] **Step 3: Implement**

```rust
pub fn cpu_pct(cpu_total: u64, precpu_total: u64, system: u64, presystem: u64, online: u64) -> f64 {
    let cpu_delta = cpu_total.saturating_sub(precpu_total) as f64;
    let system_delta = system.saturating_sub(presystem) as f64;
    if system_delta <= 0.0 || online == 0 {
        return 0.0;
    }
    (cpu_delta / system_delta) * online as f64 * 100.0
}

pub fn mem_used(usage: u64, inactive_file: u64) -> u64 {
    usage.saturating_sub(inactive_file)
}
```

- [ ] **Step 4: Run — expect pass** — `cargo test stats_math`.

- [ ] **Step 5: Commit** — `git commit -am "feat: cpu%/mem stats math"`

---

### Task 4: `resource` — bollard docker source

**Files:**
- Create: `src/resource.rs`
- Modify: `src/main.rs` (`mod resource;`)

**Interfaces:**
- Consumes: `model::RunnerResource`, `stats_math::{cpu_pct, mem_used}`, `config::Config`.
- Produces:
  - `pub struct ResourceUpdate { pub resources: Vec<RunnerResource>, pub error: Option<String> }` — what the source sends each cycle (error carries a human message when docker is unreachable; `resources` empty on error).
  - `pub struct CpuSample { pub total: u64, pub system: u64 }` — a container's cumulative CPU counters from one poll.
  - `pub fn container_matches(name: &str, prefix: &str) -> bool` — pure, trims leading `/`, checks prefix (TDD).
  - `pub fn cpu_from_samples(prev: Option<CpuSample>, cur: CpuSample, online: u64) -> f64` — first poll (`prev == None`) → `0.0`; else `stats_math::cpu_pct(cur.total, prev.total, cur.system, prev.system, online)` (TDD — this is the retention seam).
  - `pub fn connect(socket_path: &str) -> anyhow::Result<bollard::Docker>` — `Docker::connect_with_unix(socket_path, 120, bollard::API_DEFAULT_VERSION)`.
  - `pub async fn run(cfg: Config, tx: mpsc::Sender<ResourceUpdate>)` — 2s loop; owns a `HashMap<String /*container id*/, CpuSample>` of the PREVIOUS poll's samples. Each cycle: (re)connect if needed, list running containers matching `cfg.prefix`, one-shot `stats(stream=false)` per container, compute CPU% via `cpu_from_samples(prev_sample, cur_sample, online)`, update the retained map, build `Vec<RunnerResource>`, send `ResourceUpdate { resources, error: None }`. On connect/list/stats failure send `ResourceUpdate { resources: vec![], error: Some(msg) }` and keep looping (retry next cycle).

**Why retention (reviewer point 1):** bollard's one-shot `stats(stream=false)` returns a snapshot whose `precpu_stats` is **zeroed** — you cannot compute a delta from a single response. The docker CLI hides this by doing two reads internally. So `resource` must hold the previous poll's `CpuSample` per container and diff `(prev, cur)` itself, **ignoring the API's `precpu_stats` entirely**. `cpu_from_samples` isolates and tests exactly this.

- [ ] **Step 1: Failing tests for the pure helpers (incl. named retention test)** — in `src/resource.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_prefix_ignoring_leading_slash() {
        assert!(container_matches("/ci-runner-4", "ci-runner-"));
        assert!(container_matches("ci-runner-1", "ci-runner-"));
        assert!(!container_matches("other-thing", "ci-runner-"));
    }

    #[test]
    fn first_poll_zero_then_delta_from_retained_sample() {
        // First poll: no prior sample → 0% (cannot delta a single snapshot).
        let s0 = CpuSample { total: 1_000_000_000, system: 4_000_000_000 };
        assert_eq!(cpu_from_samples(None, s0, 4), 0.0);
        // Second poll: 1 full core used over the interval on a 4-core box → 100%.
        let s1 = CpuSample { total: 2_000_000_000, system: 8_000_000_000 };
        let pct = cpu_from_samples(Some(s0), s1, 4);
        assert!((pct - 100.0).abs() < 0.001, "got {pct}");
    }
}
```

- [ ] **Step 2: Run — expect fail** — `cargo test resource`.

- [ ] **Step 3: Implement** `container_matches`, `cpu_from_samples`, `connect`, `run` (retaining prev samples, ignoring `precpu_stats`):

```rust
use crate::config::Config;
use crate::model::RunnerResource;
use crate::stats_math::{cpu_pct, mem_used};
use bollard::query_parameters::{ListContainersOptionsBuilder, StatsOptionsBuilder};
use bollard::Docker;
use futures_util::stream::TryStreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy)]
pub struct CpuSample {
    pub total: u64,
    pub system: u64,
}

pub struct ResourceUpdate {
    pub resources: Vec<RunnerResource>,
    pub error: Option<String>,
}

pub fn container_matches(name: &str, prefix: &str) -> bool {
    name.trim_start_matches('/').starts_with(prefix)
}

pub fn cpu_from_samples(prev: Option<CpuSample>, cur: CpuSample, online: u64) -> f64 {
    match prev {
        None => 0.0, // first poll: no prior snapshot to delta against
        Some(p) => cpu_pct(cur.total, p.total, cur.system, p.system, online),
    }
}

pub fn connect(socket_path: &str) -> anyhow::Result<Docker> {
    Ok(Docker::connect_with_unix(socket_path, 120, bollard::API_DEFAULT_VERSION)?)
}

pub async fn run(cfg: Config, tx: mpsc::Sender<ResourceUpdate>) {
    // Retained previous-poll CPU counters, keyed by container id. IGNORE the API's
    // precpu_stats (zeroed for one-shot stats); we compute the delta ourselves.
    let mut prev: HashMap<String, CpuSample> = HashMap::new();
    let mut docker: Option<Docker> = None;
    loop {
        if docker.is_none() {
            match connect(&cfg.socket_path) {
                Ok(d) => docker = Some(d),
                Err(e) => {
                    let _ = tx
                        .send(ResourceUpdate { resources: vec![], error: Some(format!("docker: {e}")) })
                        .await;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            }
        }
        let d = docker.as_ref().unwrap();
        match collect(d, &cfg.prefix, &mut prev).await {
            Ok(resources) => {
                let _ = tx.send(ResourceUpdate { resources, error: None }).await;
            }
            Err(e) => {
                docker = None; // force reconnect next cycle
                let _ = tx
                    .send(ResourceUpdate { resources: vec![], error: Some(format!("docker: {e}")) })
                    .await;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn collect(
    d: &Docker,
    prefix: &str,
    prev: &mut HashMap<String, CpuSample>,
) -> anyhow::Result<Vec<RunnerResource>> {
    let list = d
        .list_containers(Some(ListContainersOptionsBuilder::default().build()))
        .await?;
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for c in list {
        let name = c.names.as_ref().and_then(|n| n.first()).cloned().unwrap_or_default();
        if !container_matches(&name, prefix) {
            continue;
        }
        let id = match &c.id {
            Some(id) => id.clone(),
            None => continue,
        };
        seen.push(id.clone());
        if let Ok(Some(stat)) = d
            .stats(&id, Some(StatsOptionsBuilder::default().stream(false).build()))
            .try_next()
            .await
        {
            if let Some(rr) = to_resource(&id, &name, &stat, prev) {
                out.push(rr);
            }
        }
    }
    prev.retain(|k, _| seen.contains(k)); // drop deregistered containers
    Ok(out)
}

fn to_resource(
    id: &str,
    name: &str,
    s: &bollard::models::ContainerStatsResponse,
    prev: &mut HashMap<String, CpuSample>,
) -> Option<RunnerResource> {
    let cpu = s.cpu_stats.as_ref()?;
    let mem = s.memory_stats.as_ref()?;
    let online = cpu.online_cpus.unwrap_or_else(|| {
        cpu.cpu_usage.as_ref().and_then(|u| u.percpu_usage.as_ref()).map(|v| v.len() as u64).unwrap_or(1)
    });
    let cur = CpuSample {
        total: cpu.cpu_usage.as_ref().and_then(|u| u.total_usage).unwrap_or(0),
        system: cpu.system_cpu_usage.unwrap_or(0),
    };
    let pct = cpu_from_samples(prev.get(id).copied(), cur, online);
    prev.insert(id.to_string(), cur);
    let inactive = mem.stats.as_ref().and_then(|m| m.get("inactive_file").copied()).unwrap_or(0);
    let used = mem_used(mem.usage.unwrap_or(0), inactive);
    Some(RunnerResource {
        name: name.trim_start_matches('/').to_string(),
        cpu_pct: pct,
        mem_bytes: used,
        mem_limit: mem.limit.unwrap_or(0),
    })
}
```

> Confirm exact bollard 0.20 field names/types (`ContainerStatsResponse`, `cpu_stats`, `memory_stats.stats` map) against docs.rs while implementing — adjust option-unwrapping to match. The `stats_math`/`model` contracts do not change. `cpu_from_samples` uses `stats_math::cpu_pct`; keep `#[allow(dead_code)]`-free by ensuring `cpu_pct` stays referenced.

- [ ] **Step 4: Run — expect pass (unit)** — `cargo test resource` (the two pure tests run: prefix match + retention delta).

- [ ] **Step 5: Live smoke against real docker** — add a temporary `#[tokio::main]` example or unit-gated manual check, or defer to Task 7's live run. Minimum here: `cargo build` compiles; if convenient, a throwaway binary printing one `collect()` result shows the 6 runners with **non-zero-capable** CPU% on the *second* poll (first poll shows 0.0 by design).

- [ ] **Step 6: Commit** — `git commit -am "feat: bollard rootless docker source with retained-sample cpu%"`

---

### Task 5: `jobs` — gh shell-out source

**Files:**
- Create: `src/jobs.rs`, `tests/fixtures/runs.json`, `tests/fixtures/jobs.json`
- Modify: `src/main.rs` (`mod jobs;`)

**Interfaces:**
- Consumes: `model::JobInfo`, `config::Config`.
- Produces:
  - `pub struct JobsUpdate { pub jobs: HashMap<u32, JobInfo>, pub error: Option<String> }` — sent each cycle; `error` set (and `jobs` empty) when `gh` fails/unauthenticated, so the caller can show a banner while retaining its last-known map.
  - `pub fn parse_runs(json: &str) -> Vec<(u64, String)>` — extract `workflow_runs[].{id, name}` (name = workflow label for the join).
  - `pub fn parse_jobs(workflow: &str, json: &str) -> Vec<(u32, JobInfo)>` — for each job where `status == "in_progress"` AND `runner_name` matches **exactly `^runner-\d+$`**, produce `(N, JobInfo{ workflow, job: job.name, started_at })`. Excludes GitHub-hosted (`"GitHub Actions …"`), queued (no runner_name), and completed (stale runner_name) jobs. `started_at` RFC3339 → `SystemTime` via `humantime::parse_rfc3339`.
  - `pub async fn run(cfg: Config, tx: mpsc::Sender<JobsUpdate>)` — every 15s: `gh api repos/{repo}/actions/runs?status=in_progress`; for each `(id, name)` → `gh api repos/{repo}/actions/runs/{id}/jobs`, mapping via `parse_jobs(name, …)`; send `JobsUpdate { jobs, error: None }`. On `gh` failure send `JobsUpdate { jobs: HashMap::new(), error: Some(msg) }` and keep looping.

- [ ] **Step 1: Front-load the join-key validation with REAL payloads (reviewer point 2).** Before writing any parser, capture live shapes and confirm the key:

```bash
# a run id (any status) + confirm runner_name shapes
RID=$(gh api 'repos/owner/repo/actions/runs?per_page=1' --jq '.workflow_runs[0].id')
gh api "repos/owner/repo/actions/runs/$RID/jobs" \
  --jq '.jobs[] | {name, status, runner_name, started_at}'
```

Confirmed on 2026-07-04 (bake these facts into the tests):
  - Self-hosted jobs carry `runner_name` = `runner-<N>` (e.g. `runner-4`) — the join key.
  - GitHub-hosted jobs carry `runner_name` = `"GitHub Actions 1000013810"` — **must be excluded**.
  - `status` is `completed` or `in_progress`; **completed jobs keep a stale `runner_name`** — only `in_progress` is a live runner.
  - `started_at` is RFC3339 `Z` (e.g. `2026-07-04T12:25:18Z`).

Save trimmed real payloads as fixtures containing all three job kinds:

`tests/fixtures/runs.json`:
```json
{"total_count":1,"workflow_runs":[{"id":123,"name":"Test"}]}
```
`tests/fixtures/jobs.json`:
```json
{"total_count":3,"jobs":[
  {"name":"E2E Tests","status":"in_progress","runner_name":"runner-4","started_at":"2026-07-04T12:25:18Z"},
  {"name":"Migration Execution Test","status":"completed","runner_name":"GitHub Actions 1000013810","started_at":"2026-07-04T12:22:56Z"},
  {"name":"Coverage Gate","status":"completed","runner_name":"runner-3","started_at":"2026-07-04T12:24:48Z"}
]}
```

- [ ] **Step 2: Failing tests** — `src/jobs.rs` (assert hosted + completed are excluded):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_runs_extracts_id_and_name() {
        let runs = parse_runs(include_str!("../tests/fixtures/runs.json"));
        assert_eq!(runs, vec![(123u64, "Test".to_string())]);
    }

    #[test]
    fn parse_jobs_keeps_only_in_progress_self_hosted() {
        let out = parse_jobs("Test", include_str!("../tests/fixtures/jobs.json"));
        // Only the in_progress runner-4 job survives; hosted + completed excluded.
        assert_eq!(out.len(), 1);
        let (idx, ji) = &out[0];
        assert_eq!(*idx, 4);
        assert_eq!(ji.workflow, "Test");
        assert_eq!(ji.job, "E2E Tests");
    }
}
```

- [ ] **Step 3: Run — expect fail** — `cargo test jobs`.

- [ ] **Step 4: Implement** using serde_json `Value`, tokio `Command`:

```rust
use crate::config::Config;
use crate::model::JobInfo;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio::sync::mpsc;

pub struct JobsUpdate {
    pub jobs: HashMap<u32, JobInfo>,
    pub error: Option<String>,
}

pub fn parse_runs(json: &str) -> Vec<(u64, String)> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("workflow_runs").and_then(|r| r.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| {
            let id = r.get("id")?.as_u64()?;
            let name = r.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            Some((id, name))
        })
        .collect()
}

fn parse_rfc3339(s: &str) -> SystemTime {
    humantime::parse_rfc3339(s).unwrap_or_else(|_| SystemTime::now())
}

/// Strict self-hosted key: `runner-<digits>` only. Rejects "GitHub Actions 123", "gh-runner-3", etc.
fn runner_index_strict(runner_name: &str) -> Option<u32> {
    let n = runner_name.strip_prefix("runner-")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

pub fn parse_jobs(workflow: &str, json: &str) -> Vec<(u32, JobInfo)> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    v.get("jobs")
        .and_then(|j| j.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|j| j.get("status").and_then(|s| s.as_str()) == Some("in_progress"))
                .filter_map(|j| {
                    let rn = j.get("runner_name")?.as_str()?;
                    let idx = runner_index_strict(rn)?;
                    let job = j.get("name")?.as_str()?.to_string();
                    let started = j
                        .get("started_at")
                        .and_then(|s| s.as_str())
                        .map(parse_rfc3339)
                        .unwrap_or_else(SystemTime::now);
                    Some((idx, JobInfo { workflow: workflow.to_string(), job, started_at: started }))
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn gh_api(path: &str) -> anyhow::Result<String> {
    let out = Command::new("gh").arg("api").arg(path).output().await?;
    if !out.status.success() {
        anyhow::bail!("gh api failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn poll(repo: &str) -> anyhow::Result<HashMap<u32, JobInfo>> {
    let runs_json = gh_api(&format!("repos/{repo}/actions/runs?status=in_progress")).await?;
    let mut map = HashMap::new();
    for (id, name) in parse_runs(&runs_json) {
        let jobs_json = gh_api(&format!("repos/{repo}/actions/runs/{id}/jobs")).await?;
        for (idx, ji) in parse_jobs(&name, &jobs_json) {
            map.insert(idx, ji);
        }
    }
    Ok(map)
}

pub async fn run(cfg: Config, tx: mpsc::Sender<JobsUpdate>) {
    loop {
        let update = match poll(&cfg.repo).await {
            Ok(jobs) => JobsUpdate { jobs, error: None },
            Err(e) => JobsUpdate { jobs: HashMap::new(), error: Some(format!("gh: {e}")) },
        };
        let _ = tx.send(update).await;
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}
```

> Note: `Config` isn't `Clone` by default and both sources take it by value — derive `Clone` on `Config` in Task 1, or pass the needed fields (`repo`, and for `resource` `socket_path`/`prefix`) by value. Simplest: `#[derive(Clone)]` on `Config`.

- [ ] **Step 5: Run — expect pass** — `cargo test jobs`.

- [ ] **Step 6: Commit** — `git commit -am "feat: gh jobs source (strict runner-N, in_progress only) + fixtures"`

---

### Task 6: `ui` — table + slice gauge render

**Files:**
- Create: `src/ui.rs`
- Modify: `src/main.rs` (`mod ui;`)

**Interfaces:**
- Consumes: `model::{RunnerRow, Load, elapsed_secs, slice_total_bytes}`.
- Produces:
  - `pub struct View<'a> { pub rows: &'a [RunnerRow], pub slice_cap_bytes: u64, pub now: SystemTime, pub status: Option<String> }` — `status` carries a one-line banner (docker/gh error, or `None` when healthy).
  - `pub fn render(frame: &mut ratatui::Frame, view: &View)` — draws the header (+ `status` banner in red when set), the `Table` (columns runner|CPU|mem|workflow › job|elapsed, colored by `Load`), and a bottom `Gauge` for slice total vs cap. **When `rows` is empty, render a centered empty-state line** (`"waiting for runners…"` when `status` is None, else the status message) instead of an empty table — so socket-down / gh-down / zero-runners never looks like a crash.
  - `pub fn fmt_mem(bytes: u64) -> String`, `pub fn fmt_elapsed(secs: u64) -> String` (pure, TDD).

- [ ] **Step 1: Failing tests for formatters + a TestBackend smoke test** — `src/ui.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Load, RunnerRow};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::SystemTime;

    #[test]
    fn formats_mem_and_elapsed() {
        assert_eq!(fmt_mem(1024 * 1024 * 1024), "1.0GiB");
        assert_eq!(fmt_mem(42 * 1024 * 1024), "42.0MiB");
        assert_eq!(fmt_elapsed(75), "01:15");
        assert_eq!(fmt_elapsed(3661), "1:01:01");
    }

    #[test]
    fn renders_without_panic_and_shows_runner() {
        let rows = vec![RunnerRow {
            name: "ci-runner-1".into(),
            cpu_pct: 0.5,
            mem_bytes: 47 * 1024 * 1024,
            mem_limit: 8 * 1024 * 1024 * 1024,
            job: None,
            load: Load::Idle,
        }];
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| {
            render(f, &View { rows: &rows, slice_cap_bytes: 24 * 1024 * 1024 * 1024, now: SystemTime::now(), status: None });
        })
        .unwrap();
        let content = term.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>();
        assert!(content.contains("ci-runner-1"));
        assert!(content.contains("idle"));
    }

    #[test]
    fn empty_rows_with_status_shows_banner_not_blank() {
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| {
            render(f, &View { rows: &[], slice_cap_bytes: 24 * 1024 * 1024 * 1024, now: SystemTime::now(), status: Some("docker: unreachable".into()) });
        })
        .unwrap();
        let content = term.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>();
        assert!(content.contains("docker: unreachable"));
    }
}
```

- [ ] **Step 2: Run — expect fail** — `cargo test ui`.

- [ ] **Step 3: Implement** formatters + `render` (ratatui 0.30 API: `Table::new(rows, widths)`, `Row`, `Cell`, `Gauge`, `Layout`, `Style`/`Color`, `Paragraph` for banner/empty-state). Idle → dim/gray, Busy → green, NearCap → red. Show `— idle` when `job` is None; else `{workflow} › {job}` and `fmt_elapsed`. Gauge ratio = `slice_total_bytes(rows) / slice_cap_bytes` (clamp to `0.0..=1.0`), label `X.X / N GiB` from `slice_cap_bytes`. `status` (when `Some`) renders as a red banner row; empty `rows` renders the centered empty-state `Paragraph`. (Full render code written during implementation against docs.rs/ratatui/0.30.2.)

- [ ] **Step 4: Run — expect pass** — `cargo test ui`.

- [ ] **Step 5: Commit** — `git commit -am "feat: ratatui table + slice gauge + status/empty state"`

---

### Task 7: `app` / `main` — event loop wiring

**Files:**
- Modify: `src/main.rs`
- Create: `src/app.rs`

**Interfaces:**
- Consumes: `config::Config`, `resource::{run, ResourceUpdate}`, `jobs::{run, JobsUpdate}`, `ui::{render, View}`, `model::{RunnerResource, JobInfo, join}`.
- Produces: a running TUI. `pub async fn run(mut terminal: ratatui::DefaultTerminal) -> anyhow::Result<()>`.

- [ ] **Step 1: Implement `src/app.rs`** — shared state + event loop:
  - `struct AppState { resources: Vec<RunnerResource>, jobs: HashMap<u32, JobInfo>, resource_err: Option<String>, jobs_err: Option<String> }`.
  - `Config::from_env()` once; clone into `resource::run(cfg.clone(), tx_res)` and `jobs::run(cfg.clone(), tx_jobs)` (both `tokio::spawn`).
  - Main loop uses `tokio::select!` over: crossterm `EventStream` (quit on `q`/`Ctrl-C`/`Esc`), `tx_res` receiver → set `resources` + `resource_err` from `ResourceUpdate`, `tx_jobs` receiver → set `jobs` + `jobs_err` from `JobsUpdate`, and a `tokio::time::interval(1s)` tick. **Degradation (reviewer point 5):** on a source error, keep the last-known good data but surface the message; build `status = resource_err.or(jobs_err)` (docker error takes precedence) and pass it into `View`. Redraw with `terminal.draw(|f| ui::render(f, &view))` where `view.rows = join(state.resources.clone(), &state.jobs, SystemTime::now())`. With zero runners the rows are empty and `ui` shows the empty-state — never a panic.

- [ ] **Step 2: `src/main.rs`**

```rust
mod app;
mod config;
mod jobs;
mod model;
mod resource;
mod stats_math;
mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ratatui::init installs a panic hook that restores the terminal before unwinding.
    let terminal = ratatui::init();
    let res = app::run(terminal).await;
    ratatui::restore();
    res
}
```

- [ ] **Step 3: Build + clippy** — `cargo build && cargo clippy --all-targets -- -D warnings`.

- [ ] **Step 4: Run live (happy path)** — `cargo run`. Confirm all 6 `ci-runner-*` rows appear, CPU/mem update ~2s (CPU non-zero-capable after the first poll), all show idle, slice gauge populates, `q`/`Esc` quits and restores the terminal.

- [ ] **Step 5: Verify degradation (reviewer point 5) — no panic, terminal survives:**
  - Socket down: `PITWALL_SOCKET=/nonexistent.sock cargo run` → shows a red `docker: …` banner + empty-state, `q` restores cleanly.
  - `gh` unavailable: `PATH=/usr/bin cargo run` in an env where `gh` isn't found, or temporarily with an unauthenticated `GH_TOKEN=` — jobs banner appears, resources still render, no crash.
  - Zero runners: `PITWALL_PREFIX=nomatch- cargo run` → empty-state `"waiting for runners…"`, gauge at 0, `q` restores.

- [ ] **Step 6: Commit** — `git commit -am "feat: event loop + degradation-safe live TUI"`

---

### Task 8: Install target, README, live join verification

**Files:**
- Create: `justfile` (or `Makefile`)
- Modify: `README.md`

**Interfaces:** none (packaging + docs).

- [ ] **Step 1: `justfile`**

```make
install:
    cargo build --release
    install -Dm755 target/release/pitwall ~/.local/bin/pitwall
```

- [ ] **Step 2: README** — usage: run `pitwall`, env overrides, rootless-docker note, `just install`.

- [ ] **Step 3: Live join verification** — trigger a real CI job (push a trivial commit / `gh workflow run` on `owner/repo`), run `pitwall`, and confirm the busy runner's row shows the correct `workflow › job` and a ticking `elapsed`, joined to the right `ci-runner-N`. Capture the observation.

- [ ] **Step 4: Final gates** — `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.

- [ ] **Step 5: Commit** — `git commit -am "chore: install target + README + verified live join"`

---

## Self-Review

- **Spec coverage:** stack+config (T1), model/join (T2), cpu/mem math (T3), bollard rootless source (T4), gh jobs 15s (T5), table+gauge+colors (T6), event loop + degradation + terminal restore (T7), install + live verify (T8). Non-goals honored (no native runners, no graphs, no config *file*). ✔
- **Reviewer fixes addressed:**
  1. **One-shot CPU% retention** — T4 adds `CpuSample` + `cpu_from_samples`, retains the previous poll's counters per container id, ignores the API's zeroed `precpu_stats`, and has a named test `first_poll_zero_then_delta_from_retained_sample`.
  2. **Join key front-loaded** — T5 Step 1 captures real `gh api …/jobs` payloads and records confirmed shapes (`runner-N` self-hosted, `"GitHub Actions …"` hosted, completed-retains-stale-name) before any parser is written.
  3. **Queued/completed/hosted excluded** — T5 filters runs with `?status=in_progress`, filters jobs to `in_progress`, and strict-matches `^runner-\d+$` (`runner_index_strict`); test asserts hosted + completed are dropped.
  4. **Config from the start** — T1 `src/config.rs` centralizes socket path, repo, prefix, and slice cap (env-overridable, default 24 GiB, not hardcoded at use-sites); T4/T5 consume `Config`; success criteria updated.
  5. **Degradation path** — `ResourceUpdate`/`JobsUpdate` carry `error`; T7 surfaces a status banner + keeps last-known data; T6 renders banner/empty-state; T7 Step 5 explicitly verifies socket-down / gh-down / zero-runners never panic and the terminal restores.
- **Placeholder scan:** only the `render` body (T6) and the `select!` body (T7) are prose+spec rather than full code — the parts most sensitive to exact ratatui 0.30 signatures, written against docs.rs at build time. All pure/testable logic is fully code-blocked.
- **Type consistency:** `RunnerResource`, `JobInfo`, `RunnerRow`, `Load`, `join`, `runner_index`, `cpu_pct`, `mem_used`, `Config`(`Clone`), `ResourceUpdate`, `CpuSample`, `cpu_from_samples`, `JobsUpdate`, `parse_runs → (u64,String)`, `parse_jobs`, `View{status}` are consistent across tasks.
