# pitwall Runner Stats TUI — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A btop-like Rust TUI showing, per pulse docker CI runner, live CPU/mem joined with the workflow/job currently running.

**Architecture:** Async tokio binary. Two background sources (bollard docker-stats stream; `gh` shell-out for jobs) push updates through an mpsc event channel into a shared `AppState`; a ratatui render loop reads it and draws a table + slice gauge. Pure `model` + math modules hold all testable logic.

**Tech Stack:** Rust 1.95, tokio, ratatui 0.30 + crossterm, bollard 0.20, futures-util, serde/serde_json, anyhow.

## Global Constraints

- Rust edition 2021; toolchain already installed (rustc 1.95).
- Docker access is **rootless**: socket `unix:///run/user/1000/docker.sock` (respect `DOCKER_HOST` env if set). The rootful `/var/run/docker.sock` does NOT see these containers.
- Container↔runner join: container `pulse-ci-runner-N` ↔ GitHub `runner_name` `runner-N`, keyed on trailing integer N.
- Container up + no in-progress GitHub job = **idle** (expected steady state, never an error).
- Jobs polled every 15s (rate-limit safe); docker stats stream continuous.
- Defaults overridable by env: `PITWALL_REPO` = `erwins-enkel/pulse`, `PITWALL_PREFIX` = `pulse-ci-runner-`, `PITWALL_SLICE_CAP_GIB` = `24`.
- `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` must all pass; never suppress warnings to pass.
- Binary installs to `~/.local/bin/pitwall`.

---

### Task 1: Project scaffold + lint gate

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `rustfmt.toml`, `.githooks/pre-commit`
- Modify: `.gitignore`

**Interfaces:**
- Produces: a buildable binary crate named `pitwall` with all deps declared; a committed pre-commit hook running fmt+clippy+test.

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

- [ ] **Step 7: Verify** — `cargo build && cargo clippy --all-targets -- -D warnings && cargo fmt --check`. Expected: clean build, no warnings.

- [ ] **Step 8: Commit** — `git add -A && git commit -m "chore: scaffold pitwall rust crate + lint hook"`

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
  - `pub fn runner_index(name: &str) -> Option<u32>` — parses trailing integer from `pulse-ci-runner-4` → `4`.
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
        assert_eq!(runner_index("pulse-ci-runner-4"), Some(4));
        assert_eq!(runner_index("runner-2"), Some(2));
        assert_eq!(runner_index("nope"), None);
    }

    #[test]
    fn no_job_is_idle() {
        let rows = join(vec![res("pulse-ci-runner-1", 100)], &HashMap::new(), SystemTime::now());
        assert!(matches!(rows[0].load, Load::Idle));
        assert!(rows[0].job.is_none());
    }

    #[test]
    fn job_present_is_busy() {
        let now = SystemTime::now();
        let mut jobs = HashMap::new();
        jobs.insert(1u32, JobInfo { workflow: "ci".into(), job: "test".into(), started_at: now - Duration::from_secs(30) });
        let rows = join(vec![res("pulse-ci-runner-1", 100)], &jobs, now);
        assert!(matches!(rows[0].load, Load::Busy));
        assert_eq!(elapsed_secs(rows[0].job.as_ref().unwrap().started_at, now), 30);
    }

    #[test]
    fn high_mem_is_near_cap() {
        let cap = 8u64 * 1024 * 1024 * 1024;
        let rows = join(vec![res("pulse-ci-runner-1", (cap as f64 * 0.95) as u64)], &HashMap::new(), SystemTime::now());
        assert!(matches!(rows[0].load, Load::NearCap));
    }

    #[test]
    fn rows_sorted_by_index_and_slice_summed() {
        let rows = join(vec![res("pulse-ci-runner-3", 300), res("pulse-ci-runner-1", 100)], &HashMap::new(), SystemTime::now());
        assert_eq!(rows[0].name, "pulse-ci-runner-1");
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
- Consumes: `model::RunnerResource`, `stats_math::{cpu_pct, mem_used}`.
- Produces:
  - `pub fn connect() -> anyhow::Result<bollard::Docker>` — honors `DOCKER_HOST` (strip `unix://`), else `/run/user/{uid}/docker.sock` via `Docker::connect_with_unix(path, 120, bollard::API_DEFAULT_VERSION)`.
  - `pub fn container_matches(name: &str, prefix: &str) -> bool` — pure, trims leading `/`, checks prefix (TDD this one).
  - `pub async fn run(docker: Docker, prefix: String, tx: mpsc::Sender<Vec<RunnerResource>>)` — every ~2s lists running containers matching prefix, one-shot `stats` per container, builds `Vec<RunnerResource>`, sends via `tx`. On error: log to stderr-less buffer (ignore), keep looping.

Note on the streaming-vs-poll tradeoff: bollard's per-container `stream(true)` gives one task per container. For 6 containers a **2s one-shot poll loop** (`stream(false)`) is simpler, still smooth enough, and yields cpu delta from the stat's own `precpu_stats`. Use the one-shot poll; it keeps this task a single tokio task and avoids per-container task lifecycle. (Streaming remains a later optimization; not needed for v1 feel.)

- [ ] **Step 1: Failing test for the pure helper** — in `src/resource.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_prefix_ignoring_leading_slash() {
        assert!(container_matches("/pulse-ci-runner-4", "pulse-ci-runner-"));
        assert!(container_matches("pulse-ci-runner-1", "pulse-ci-runner-"));
        assert!(!container_matches("other-thing", "pulse-ci-runner-"));
    }
}
```

- [ ] **Step 2: Run — expect fail** — `cargo test resource`.

- [ ] **Step 3: Implement** `container_matches`, `connect`, `run`:

```rust
use crate::model::RunnerResource;
use crate::stats_math::{cpu_pct, mem_used};
use bollard::query_parameters::{ListContainersOptionsBuilder, StatsOptionsBuilder};
use bollard::Docker;
use futures_util::stream::TryStreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

pub fn container_matches(name: &str, prefix: &str) -> bool {
    name.trim_start_matches('/').starts_with(prefix)
}

pub fn connect() -> anyhow::Result<Docker> {
    let path = std::env::var("DOCKER_HOST")
        .ok()
        .map(|h| h.trim_start_matches("unix://").to_string())
        .unwrap_or_else(|| format!("/run/user/{}/docker.sock", nix_uid()));
    Ok(Docker::connect_with_unix(&path, 120, bollard::API_DEFAULT_VERSION)?)
}

fn nix_uid() -> u32 {
    // Avoid extra deps: read from env or default to 1000.
    std::env::var("UID").ok().and_then(|s| s.parse().ok()).unwrap_or(1000)
}

pub async fn run(docker: Docker, prefix: String, tx: mpsc::Sender<Vec<RunnerResource>>) {
    loop {
        if let Ok(list) = docker
            .list_containers(Some(ListContainersOptionsBuilder::default().build()))
            .await
        {
            let mut out = Vec::new();
            for c in list {
                let name = c
                    .names
                    .as_ref()
                    .and_then(|n| n.first())
                    .cloned()
                    .unwrap_or_default();
                if !container_matches(&name, &prefix) {
                    continue;
                }
                let id = match &c.id {
                    Some(id) => id.clone(),
                    None => continue,
                };
                if let Ok(Some(stat)) = docker
                    .stats(&id, Some(StatsOptionsBuilder::default().stream(false).build()))
                    .try_next()
                    .await
                {
                    if let Some(rr) = to_resource(&name, &stat) {
                        out.push(rr);
                    }
                }
            }
            let _ = tx.send(out).await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn to_resource(name: &str, s: &bollard::models::ContainerStatsResponse) -> Option<RunnerResource> {
    let cpu = s.cpu_stats.as_ref()?;
    let pre = s.precpu_stats.as_ref()?;
    let mem = s.memory_stats.as_ref()?;
    let online = cpu.online_cpus.unwrap_or_else(|| {
        cpu.cpu_usage.as_ref().and_then(|u| u.percpu_usage.as_ref()).map(|v| v.len() as u64).unwrap_or(1)
    });
    let pct = cpu_pct(
        cpu.cpu_usage.as_ref().and_then(|u| u.total_usage).unwrap_or(0),
        pre.cpu_usage.as_ref().and_then(|u| u.total_usage).unwrap_or(0),
        cpu.system_cpu_usage.unwrap_or(0),
        pre.system_cpu_usage.unwrap_or(0),
        online,
    );
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

> The exact bollard 0.20 field names (`ContainerStatsResponse`, `cpu_stats`, `memory_stats.stats` map type) must be confirmed against the crate's docs.rs while implementing — adjust option-unwrapping to match. The `stats_math`/`model` contracts do not change.

- [ ] **Step 4: Run — expect pass (unit)** — `cargo test resource` (only the pure test runs).

- [ ] **Step 5: Manual smoke** — a throwaway `main` or `cargo run` wiring is not required yet; confirm compile with `cargo build`.

- [ ] **Step 6: Commit** — `git commit -am "feat: bollard rootless docker resource source"`

---

### Task 5: `jobs` — gh shell-out source

**Files:**
- Create: `src/jobs.rs`, `tests/fixtures/runs.json`, `tests/fixtures/jobs.json`
- Modify: `src/main.rs` (`mod jobs;`)

**Interfaces:**
- Consumes: `model::JobInfo`.
- Produces:
  - `pub fn parse_runs(json: &str) -> Vec<u64>` — extract `workflow_runs[].id`.
  - `pub fn parse_jobs(runs_workflow: &str, json: &str) -> Vec<(u32, JobInfo)>` — from a run's jobs payload, for each job with a `runner_name` matching `runner-N` and `status == "in_progress"`, produce `(N, JobInfo{ workflow, job: job.name, started_at })`. `started_at` from `started_at` RFC3339 → SystemTime.
  - `pub async fn run(repo: String, tx: mpsc::Sender<HashMap<u32, JobInfo>>)` — every 15s: `gh api repos/{repo}/actions/runs?status=in_progress`, for each run id `gh api repos/{repo}/actions/runs/{id}/jobs`, aggregate into a map, send. On any error keep the loop (caller retains last map).

- [ ] **Step 1: Capture fixtures** — save a real (or minimal representative) payload:

`tests/fixtures/runs.json`:
```json
{"total_count":1,"workflow_runs":[{"id":123,"name":"ci"}]}
```
`tests/fixtures/jobs.json`:
```json
{"total_count":1,"jobs":[{"name":"test","status":"in_progress","runner_name":"runner-4","started_at":"2026-07-04T10:00:00Z"}]}
```

- [ ] **Step 2: Failing tests** — `src/jobs.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_runs_extracts_ids() {
        let ids = parse_runs(include_str!("../tests/fixtures/runs.json"));
        assert_eq!(ids, vec![123]);
    }

    #[test]
    fn parse_jobs_maps_runner_index() {
        let out = parse_jobs("ci", include_str!("../tests/fixtures/jobs.json"));
        assert_eq!(out.len(), 1);
        let (idx, ji) = &out[0];
        assert_eq!(*idx, 4);
        assert_eq!(ji.workflow, "ci");
        assert_eq!(ji.job, "test");
    }
}
```

- [ ] **Step 3: Run — expect fail** — `cargo test jobs`.

- [ ] **Step 4: Implement** using serde_json `Value`, tokio `Command`:

```rust
use crate::model::JobInfo;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio::sync::mpsc;

pub fn parse_runs(json: &str) -> Vec<u64> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("workflow_runs").and_then(|r| r.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r.get("id").and_then(|i| i.as_u64()))
        .collect()
}

fn parse_rfc3339(s: &str) -> SystemTime {
    // Minimal: parse "YYYY-MM-DDTHH:MM:SSZ" to epoch. Prefer a tiny helper over chrono.
    // Implementation detail: convert via time components; if parse fails, return now().
    humantime::parse_rfc3339(s).unwrap_or_else(|_| SystemTime::now())
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
                    let idx: u32 = rn.rsplit('-').next()?.parse().ok()?;
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
        anyhow::bail!("gh api failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub async fn run(repo: String, tx: mpsc::Sender<HashMap<u32, JobInfo>>) {
    loop {
        if let Ok(runs_json) = gh_api(&format!("repos/{repo}/actions/runs?status=in_progress")).await {
            let mut map = HashMap::new();
            for id in parse_runs(&runs_json) {
                // workflow name: pull from the run object; simple approach re-fetches nothing —
                // use the run's "name" via a second pass or store from parse. For v1 use run name lookup:
                if let Ok(jobs_json) = gh_api(&format!("repos/{repo}/actions/runs/{id}/jobs")).await {
                    // workflow label: best-effort from jobs payload's first job's workflow_name if present,
                    // else the run name captured in parse_runs (extend parse_runs to return (id,name)).
                    for (idx, ji) in parse_jobs("", &jobs_json) {
                        map.insert(idx, ji);
                    }
                }
            }
            let _ = tx.send(map).await;
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}
```

> Decisions to lock while implementing: (a) add `humantime = "2"` to Cargo.toml for RFC3339 parsing (small, no chrono); (b) to fill `workflow`, change `parse_runs` to return `Vec<(u64, String)>` (id, run name) and thread the name into `parse_jobs` — update Task 5 tests accordingly if you make this change. Keep the `parse_jobs(workflow, json)` signature as the tested contract.

- [ ] **Step 5: Run — expect pass** — `cargo test jobs`.

- [ ] **Step 6: Commit** — `git commit -am "feat: gh jobs source + fixtures"`

---

### Task 6: `ui` — table + slice gauge render

**Files:**
- Create: `src/ui.rs`
- Modify: `src/main.rs` (`mod ui;`)

**Interfaces:**
- Consumes: `model::{RunnerRow, Load, elapsed_secs, slice_total_bytes}`.
- Produces:
  - `pub struct View<'a> { pub rows: &'a [RunnerRow], pub slice_cap_bytes: u64, pub now: SystemTime, pub stale_jobs: bool }`
  - `pub fn render(frame: &mut ratatui::Frame, view: &View)` — draws a `Table` (columns runner|CPU|mem|workflow › job|elapsed, colored by `Load`) and a bottom `Gauge` for slice total vs cap.
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
            name: "pulse-ci-runner-1".into(),
            cpu_pct: 0.5,
            mem_bytes: 47 * 1024 * 1024,
            mem_limit: 8 * 1024 * 1024 * 1024,
            job: None,
            load: Load::Idle,
        }];
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| {
            render(f, &View { rows: &rows, slice_cap_bytes: 24 * 1024 * 1024 * 1024, now: SystemTime::now(), stale_jobs: false });
        })
        .unwrap();
        let content = term.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>();
        assert!(content.contains("pulse-ci-runner-1"));
        assert!(content.contains("idle"));
    }
}
```

- [ ] **Step 2: Run — expect fail** — `cargo test ui`.

- [ ] **Step 3: Implement** formatters + `render` (ratatui 0.30 API: `Table::new(rows, widths)`, `Row`, `Cell`, `Gauge`, `Layout`, `Style`/`Color`). Idle → dim/gray, Busy → green, NearCap → red. Show `— idle` when `job` is None; else `{workflow} › {job}` and `fmt_elapsed`. Gauge ratio = `slice_total_bytes(rows) / slice_cap_bytes`, label `X.X / 24 GiB`. If `stale_jobs`, add a header note. (Full render code written during implementation against docs.rs/ratatui/0.30.2.)

- [ ] **Step 4: Run — expect pass** — `cargo test ui`.

- [ ] **Step 5: Commit** — `git commit -am "feat: ratatui table + slice gauge"`

---

### Task 7: `app` / `main` — event loop wiring

**Files:**
- Modify: `src/main.rs`
- Create: `src/app.rs`

**Interfaces:**
- Consumes: `resource::run`, `jobs::run`, `ui::{render, View}`, `model::{RunnerResource, JobInfo, join}`.
- Produces: a running TUI. `#[tokio::main] async fn main()`.

- [ ] **Step 1: Implement `src/app.rs`** — shared state + event loop:
  - `struct AppState { resources: Vec<RunnerResource>, jobs: HashMap<u32, JobInfo>, jobs_stale: bool }`.
  - Spawn `resource::run` (tx_res) and `jobs::run` (tx_jobs).
  - Main loop uses `tokio::select!` over: crossterm `EventStream` (quit on `q`/`Ctrl-C`/`Esc`), `tx_res` receiver, `tx_jobs` receiver, and a `tokio::time::interval(1s)` tick. On each, update state and redraw with `terminal.draw(|f| ui::render(f, &view))` where `view.rows = join(state.resources.clone(), &state.jobs, now)`.
  - Config read from env (`PITWALL_REPO`, `PITWALL_PREFIX`, `PITWALL_SLICE_CAP_GIB`).

- [ ] **Step 2: `src/main.rs`**

```rust
mod app;
mod jobs;
mod model;
mod resource;
mod stats_math;
mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let terminal = ratatui::init();
    let res = app::run(terminal).await;
    ratatui::restore();
    res
}
```

- [ ] **Step 3: Build + clippy** — `cargo build && cargo clippy --all-targets -- -D warnings`.

- [ ] **Step 4: Run live** — `cargo run`. Confirm all 6 `pulse-ci-runner-*` rows appear, CPU/mem update ~2s, all show idle, slice gauge populates, `q` quits and restores the terminal.

- [ ] **Step 5: Commit** — `git commit -am "feat: event loop + live TUI"`

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

- [ ] **Step 3: Live join verification** — trigger a real pulse CI job (push a trivial commit / `gh workflow run` on `erwins-enkel/pulse`), run `pitwall`, and confirm the busy runner's row shows the correct `workflow › job` and a ticking `elapsed`, joined to the right `pulse-ci-runner-N`. Capture the observation.

- [ ] **Step 4: Final gates** — `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.

- [ ] **Step 5: Commit** — `git commit -am "chore: install target + README + verified live join"`

---

## Self-Review

- **Spec coverage:** stack (T1), model/join (T2), cpu/mem math (T3), bollard rootless source + abstraction seam (T4 — source is a plain module; trait extraction deferred until a 2nd source exists, per YAGNI), gh jobs 15s (T5), table+gauge+colors (T6), event loop + error resilience + terminal restore (T7), install + live verify (T8). Non-goals honored (no native runners, no graphs, no config file). ✔
- **Placeholder scan:** render code in T6 and app loop in T7 are described, not fully code-blocked, because they are the parts most sensitive to exact ratatui 0.30 signatures — implementer writes them against docs.rs at build time. All pure/testable logic IS fully code-blocked. Acceptable.
- **Type consistency:** `RunnerResource`, `JobInfo`, `RunnerRow`, `Load`, `join`, `runner_index`, `cpu_pct`, `mem_used`, `parse_runs`, `parse_jobs` signatures are consistent across tasks. The `parse_runs → (id,name)` and `humantime` dep are flagged as implement-time decisions with the tested contract held fixed.
