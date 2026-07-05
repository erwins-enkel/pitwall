# Hosted-runner Status Section Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show GitHub-hosted runner activity in pitwall as a separate "hosted" section listing in-progress and queued hosted jobs (workflow › job, label, branch, elapsed/wait) for the configured repos.

**Architecture:** The jobs poller already fetches per-repo job data. We add a queued-runs poll, classify each job as self-hosted (has the `self-hosted` label) or hosted, route hosted jobs to a new `Vec<HostedJob>` that flows — with per-scope last-known-good preservation — through `JobsUpdate` → `AppState` → `View`, and render it in a new UI section between the runner table and the gauge. The self-hosted `RunnerKey`/`join` path is untouched.

**Tech Stack:** Rust, tokio, ratatui 0.30, serde_json, `gh` CLI (job data).

## Global Constraints

- Edition 2021; no new dependencies.
- No new config keys/env vars — the feature is always on, shown only when hosted jobs exist.
- Hosted jobs are sourced from **repo** scopes only; org scopes contribute nothing to the hosted list.
- Discriminator: a job is self-hosted iff its `labels` array contains the exact string `"self-hosted"`.
- Queued rows show wait time from `created_at`; running rows show elapsed from `started_at`.
- Colors come from the existing `Palette` roles (`busy` for running, `warn` for queued) — no new palette entries.
- Follow existing style: `cargo fmt`, `cargo clippy` clean, tests via `cargo test`. The pre-commit hook runs fmt+clippy+test on every commit.

---

### Task 1: `HostedJob` model + sort

**Files:**
- Modify: `src/model.rs` (add types + `sort_hosted`, near `JobInfo`)

**Interfaces:**
- Consumes: existing `elapsed_secs(started, now)` from `model.rs`.
- Produces:
  - `pub enum HostedStatus { InProgress, Queued }` (derives `Debug, Clone, Copy, PartialEq, Eq`)
  - `pub struct HostedJob { pub workflow: String, pub job: String, pub label: String, pub branch: String, pub status: HostedStatus, pub since: SystemTime }` (derives `Debug, Clone`)
  - `pub fn sort_hosted(jobs: &mut [HostedJob], now: SystemTime)`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/model.rs`:

```rust
#[test]
fn sort_hosted_running_first_then_longest_first() {
    let now = SystemTime::now();
    fn j(job: &str, status: HostedStatus, ago: u64, now: SystemTime) -> HostedJob {
        HostedJob {
            workflow: "w".into(),
            job: job.into(),
            label: "ubuntu-latest".into(),
            branch: "main".into(),
            status,
            since: now - Duration::from_secs(ago),
        }
    }
    let mut v = vec![
        j("q-new", HostedStatus::Queued, 5, now),
        j("run-new", HostedStatus::InProgress, 10, now),
        j("q-old", HostedStatus::Queued, 90, now),
        j("run-old", HostedStatus::InProgress, 120, now),
    ];
    sort_hosted(&mut v, now);
    let order: Vec<&str> = v.iter().map(|h| h.job.as_str()).collect();
    // running first (longest elapsed first), then queued (longest wait first)
    assert_eq!(order, vec!["run-old", "run-new", "q-old", "q-new"]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib model::tests::sort_hosted_running_first_then_longest_first`
Expected: FAIL — `cannot find type HostedJob` / `HostedStatus` / function `sort_hosted`.

- [ ] **Step 3: Write minimal implementation**

Add to `src/model.rs` (after the `JobInfo` struct):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedStatus {
    InProgress,
    Queued,
}

/// A GitHub-hosted job (running or queued) shown in the hosted section. Hosted
/// runners are ephemeral per-job VMs with no obtainable CPU/mem, so this carries
/// only job-level facts. `since` is `started_at` for running jobs and
/// `created_at` for queued jobs, so `elapsed_secs(since, now)` yields elapsed or
/// wait time respectively.
#[derive(Debug, Clone)]
pub struct HostedJob {
    pub workflow: String,
    pub job: String,
    pub label: String,
    pub branch: String,
    pub status: HostedStatus,
    pub since: SystemTime,
}

fn hosted_status_order(s: HostedStatus) -> u8 {
    match s {
        HostedStatus::InProgress => 0,
        HostedStatus::Queued => 1,
    }
}

/// Sort running jobs before queued, and within each group longest-first
/// (largest elapsed/wait). Stable ordering for the hosted section.
pub fn sort_hosted(jobs: &mut [HostedJob], now: SystemTime) {
    jobs.sort_by(|a, b| {
        hosted_status_order(a.status)
            .cmp(&hosted_status_order(b.status))
            .then_with(|| elapsed_secs(b.since, now).cmp(&elapsed_secs(a.since, now)))
    });
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib model::tests::sort_hosted_running_first_then_longest_first`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/model.rs
git commit -m "feat: HostedJob model + sort_hosted"
```

---

### Task 2: `parse_hosted_jobs` parser + fixture

**Files:**
- Create: `tests/fixtures/hosted_jobs.json`
- Modify: `src/jobs.rs` (add `is_self_hosted`, `parse_hosted_jobs`; import the new model types)

**Interfaces:**
- Consumes: `HostedJob`, `HostedStatus` (Task 1); existing `parse_rfc3339` in `jobs.rs`.
- Produces: `pub fn parse_hosted_jobs(workflow: &str, branch: &str, json: &str) -> Vec<HostedJob>`

- [ ] **Step 1: Create the fixture**

Create `tests/fixtures/hosted_jobs.json` (mixed: self-hosted running, hosted running, hosted queued, completed hosted):

```json
{"total_count":4,"jobs":[
  {"name":"E2E Tests","status":"in_progress","runner_name":"runner-4","labels":["self-hosted","linux","x64"],"created_at":"2026-07-04T12:24:00Z","started_at":"2026-07-04T12:25:18Z"},
  {"name":"Build","status":"in_progress","runner_name":"GitHub Actions 12","labels":["ubuntu-latest"],"created_at":"2026-07-04T12:25:00Z","started_at":"2026-07-04T12:26:00Z"},
  {"name":"Lint","status":"queued","runner_name":null,"labels":["ubuntu-24.04"],"created_at":"2026-07-04T12:26:30Z","started_at":null},
  {"name":"Old Job","status":"completed","runner_name":"GitHub Actions 9","labels":["ubuntu-latest"],"created_at":"2026-07-04T12:20:00Z","started_at":"2026-07-04T12:21:00Z"}
]}
```

- [ ] **Step 2: Write the failing test**

Add to the `tests` module in `src/jobs.rs`:

```rust
#[test]
fn parse_hosted_jobs_keeps_hosted_running_and_queued_only() {
    let out = parse_hosted_jobs("CI", "main", include_str!("../tests/fixtures/hosted_jobs.json"));
    // self-hosted (E2E Tests) excluded; completed (Old Job) excluded.
    assert_eq!(out.len(), 2);

    let build = out.iter().find(|h| h.job == "Build").unwrap();
    assert_eq!(build.workflow, "CI");
    assert_eq!(build.branch, "main");
    assert_eq!(build.label, "ubuntu-latest");
    assert_eq!(build.status, HostedStatus::InProgress);
    // running → since == started_at (12:26:00Z)
    assert_eq!(build.since, parse_rfc3339("2026-07-04T12:26:00Z"));

    let lint = out.iter().find(|h| h.job == "Lint").unwrap();
    assert_eq!(lint.label, "ubuntu-24.04");
    assert_eq!(lint.status, HostedStatus::Queued);
    // queued → since == created_at (12:26:30Z)
    assert_eq!(lint.since, parse_rfc3339("2026-07-04T12:26:30Z"));
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib jobs::tests::parse_hosted_jobs_keeps_hosted_running_and_queued_only`
Expected: FAIL — `cannot find function parse_hosted_jobs`.

- [ ] **Step 4: Write minimal implementation**

In `src/jobs.rs`, extend the model import at the top:

```rust
use crate::model::{HostedJob, HostedStatus, JobInfo, RunnerKey};
```

Add (near `parse_jobs`):

```rust
/// True if a GitHub job's `labels` array marks it self-hosted. GitHub auto-adds
/// the `self-hosted` label to every self-hosted runner job; hosted jobs never
/// carry it. A missing/!array `labels` is treated as not-self-hosted (hosted).
fn is_self_hosted(labels: &serde_json::Value) -> bool {
    labels
        .as_array()
        .is_some_and(|arr| arr.iter().any(|l| l.as_str() == Some("self-hosted")))
}

/// Hosted (non-self-hosted) jobs in status `in_progress`/`queued` from a run's
/// jobs payload. `since` is `started_at` for running jobs, `created_at` for
/// queued. `label` is the first requested label (e.g. `ubuntu-latest`).
pub fn parse_hosted_jobs(workflow: &str, branch: &str, json: &str) -> Vec<HostedJob> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    v.get("jobs")
        .and_then(|j| j.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|j| {
                    let status = match j.get("status").and_then(|s| s.as_str())? {
                        "in_progress" => HostedStatus::InProgress,
                        "queued" => HostedStatus::Queued,
                        _ => return None,
                    };
                    let labels = j.get("labels").cloned().unwrap_or(serde_json::Value::Null);
                    if is_self_hosted(&labels) {
                        return None;
                    }
                    let job = j.get("name")?.as_str()?.to_string();
                    let label = labels
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(|l| l.as_str())
                        .unwrap_or("hosted")
                        .to_string();
                    let ts_key = match status {
                        HostedStatus::InProgress => "started_at",
                        HostedStatus::Queued => "created_at",
                    };
                    let since = j
                        .get(ts_key)
                        .and_then(|s| s.as_str())
                        .map(parse_rfc3339)
                        .unwrap_or_else(SystemTime::now);
                    Some(HostedJob {
                        workflow: workflow.to_string(),
                        job,
                        label,
                        branch: branch.to_string(),
                        status,
                        since,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib jobs::tests::parse_hosted_jobs_keeps_hosted_running_and_queued_only`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/jobs.rs tests/fixtures/hosted_jobs.json
git commit -m "feat: parse_hosted_jobs + self-hosted label discriminator"
```

---

### Task 3: Poller wiring — queued runs, hosted collection, per-scope preservation

**Files:**
- Modify: `src/jobs.rs` (`ScopeState`, `poll_repo`, `poll_org`, `ScopeOutcome`, `merge_scopes`, `flatten`, `run`, `JobsUpdate`; update existing merge tests to the `ScopeState` wrapper)

**Interfaces:**
- Consumes: `parse_hosted_jobs` (Task 2), `sort_hosted` (Task 1), existing `parse_runs`, `parse_jobs`, `gh_api`.
- Produces:
  - `JobsUpdate { pub jobs: Slice, pub hosted: Vec<HostedJob>, pub error: Option<String> }`
  - Internal `ScopeState { slice: Slice, hosted: Vec<HostedJob> }` carried by `ScopeOutcome::Ok`.

**Note on churn:** `merge_scopes`/`flatten`/`ScopeOutcome` now carry `ScopeState` instead of a bare `Slice`. The self-hosted join *logic* is unchanged; only the container gains a sibling `hosted` field. The three existing merge tests get mechanically wrapped (`slice_with(..)` → a `ScopeState` with that slice + empty hosted).

- [ ] **Step 1: Write the failing test (hosted preservation on repo error)**

Add to the `tests` module in `src/jobs.rs`:

```rust
fn scope_state_with_hosted(job: &str) -> ScopeState {
    ScopeState {
        slice: Slice::new(),
        hosted: vec![HostedJob {
            workflow: "w".into(),
            job: job.into(),
            label: "ubuntu-latest".into(),
            branch: "main".into(),
            status: HostedStatus::InProgress,
            since: SystemTime::now(),
        }],
    }
}

#[test]
fn merge_repo_error_preserves_prior_hosted() {
    let mut prev = HashMap::new();
    prev.insert("o/r".to_string(), scope_state_with_hosted("Build"));

    // Repo poll fails → keep prior scope state (hosted included) + banner.
    let (next, err) = merge_scopes(prev, vec![("o/r".to_string(), ScopeOutcome::RepoErr)]);

    assert_eq!(next["o/r"].hosted.len(), 1);
    assert_eq!(next["o/r"].hosted[0].job, "Build");
    assert_eq!(err.as_deref(), Some("gh: o/r"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib jobs::tests::merge_repo_error_preserves_prior_hosted`
Expected: FAIL — `cannot find type ScopeState` / field errors (and the existing merge tests won't compile yet against the new signature).

- [ ] **Step 3: Implement the `ScopeState` refactor**

In `src/jobs.rs`:

1. Add the struct (near the `Slice` type alias):

```rust
/// Per-scope poll result: the self-hosted runner slice plus hosted jobs. Kept as
/// the last-known-good unit so a failed poll preserves both together.
#[derive(Default, Clone)]
pub struct ScopeState {
    pub slice: Slice,
    pub hosted: Vec<HostedJob>,
}
```

2. Change `ScopeOutcome::Ok` to carry `ScopeState`:

```rust
pub enum ScopeOutcome {
    /// Fresh data — replaces the scope's prior state (empty ⇒ clears it).
    Ok(ScopeState),
    RepoErr,
    OrgSkip,
}
```

3. `merge_scopes` — change the `prev`/return maps to `HashMap<String, ScopeState>`:

```rust
pub fn merge_scopes(
    mut prev: HashMap<String, ScopeState>,
    results: Vec<(String, ScopeOutcome)>,
) -> (HashMap<String, ScopeState>, Option<String>) {
    let mut failed: Vec<String> = Vec::new();
    for (scope, outcome) in results {
        match outcome {
            ScopeOutcome::Ok(state) => {
                prev.insert(scope, state);
            }
            ScopeOutcome::RepoErr => failed.push(scope),
            ScopeOutcome::OrgSkip => {}
        }
    }
    let err = if failed.is_empty() {
        None
    } else {
        Some(format!("gh: {}", failed.join(", ")))
    };
    (prev, err)
}
```

4. `flatten` — return both the unioned slice and concatenated hosted list:

```rust
fn flatten(per_scope: &HashMap<String, ScopeState>) -> (Slice, Vec<HostedJob>) {
    let mut slice = Slice::new();
    let mut hosted = Vec::new();
    for state in per_scope.values() {
        for (k, v) in &state.slice {
            slice.insert(k.clone(), v.clone());
        }
        hosted.extend(state.hosted.iter().cloned());
    }
    (slice, hosted)
}
```

5. `poll_repo` — poll both statuses, fetch each run's jobs once, split self-hosted vs hosted:

```rust
async fn poll_repo(repo: &str) -> anyhow::Result<ScopeState> {
    let mut st = ScopeState::default();
    for status in ["in_progress", "queued"] {
        let runs_json =
            gh_api(&format!("repos/{repo}/actions/runs?status={status}")).await?;
        for (id, name, branch) in parse_runs(&runs_json) {
            let jobs_json =
                gh_api(&format!("repos/{repo}/actions/runs/{id}/jobs")).await?;
            for (runner_name, ji) in parse_jobs(&name, &branch, &jobs_json) {
                st.slice.insert(
                    RunnerKey {
                        scope: repo.to_string(),
                        name: runner_name,
                    },
                    Some(ji),
                );
            }
            st.hosted
                .extend(parse_hosted_jobs(&name, &branch, &jobs_json));
        }
    }
    Ok(st)
}
```

6. `poll_org` — wrap its slice in a `ScopeState` (org scopes never contribute hosted jobs):

```rust
async fn poll_org(org: &str) -> Option<ScopeState> {
    let json = gh_api(&format!("orgs/{org}/actions/runners")).await.ok()?;
    let mut slice = Slice::new();
    for name in parse_org_runners(&json) {
        slice.insert(
            RunnerKey {
                scope: org.to_string(),
                name,
            },
            None,
        );
    }
    Some(ScopeState {
        slice,
        hosted: Vec::new(),
    })
}
```

7. `JobsUpdate` — add the `hosted` field:

```rust
pub struct JobsUpdate {
    pub jobs: Slice,
    pub hosted: Vec<HostedJob>,
    pub error: Option<String>,
}
```

8. `run` — change `prev` to `HashMap<String, ScopeState>`; flatten, sort, and send hosted. Update the two `tx.send(JobsUpdate { .. })` sites:

The unpollable-config branch:

```rust
let _ = tx
    .send(JobsUpdate {
        jobs: Slice::new(),
        hosted: Vec::new(),
        error: Some(
            "PITWALL_REPO unset — set it to your runners' repo (e.g. myorg/myrepo)".into(),
        ),
    })
    .await;
```

The main send (replacing the current flatten call):

```rust
let (next, error) = merge_scopes(std::mem::take(&mut prev), results);
prev = next;
let (jobs, mut hosted) = flatten(&prev);
sort_hosted(&mut hosted, SystemTime::now());
let _ = tx.send(JobsUpdate { jobs, hosted, error }).await;
```

Change the `prev` binding at the top of `run` to:

```rust
let mut prev: HashMap<String, ScopeState> = HashMap::new();
```

9. Keep the build green: `app.rs`'s `jobs_update_always_replaces` test constructs a `JobsUpdate { jobs, error }` literal that now misses the `hosted` field. Add `hosted: Vec::new(),` to it (this is the only `JobsUpdate` literal outside `jobs.rs`). `cargo test` compiles all targets, so this must land in this commit.

- [ ] **Step 4: Update the three existing merge tests to the `ScopeState` wrapper**

In `src/jobs.rs` tests, replace the `slice_with` helper usages so `prev` holds `ScopeState`. Change the helper to build a `ScopeState`:

```rust
fn state_with(scope: &str, name: &str) -> ScopeState {
    let mut s = Slice::new();
    s.insert(
        RunnerKey {
            scope: scope.into(),
            name: name.into(),
        },
        Some(JobInfo {
            workflow: "w".into(),
            job: "j".into(),
            branch: "main".into(),
            started_at: SystemTime::now(),
        }),
    );
    ScopeState {
        slice: s,
        hosted: Vec::new(),
    }
}
```

Then in the three affected tests:
- `merge_repo_failure_preserves_prior_and_names_scope`: `prev.insert("scoop/vanscout", state_with("scoop/vanscout", "backontop-vanscout"))` etc.; the `ScopeOutcome::Ok(Slice::new())` becomes `ScopeOutcome::Ok(ScopeState::default())`; assert on `next["scoop/vanscout"].slice.contains_key(..)` and `next["scoop/kanban-api"].slice.is_empty()`.
- `merge_org_failure_is_silent_and_preserves`: wrap the org slice in `ScopeState { slice: org_slice, hosted: vec![] }`; assert `next["ltdovr"].slice.contains_key(..)`.
- `merge_flatten_unions_all_scopes`: use `state_with`/wrapped org slice; `let (flat, _hosted) = flatten(&prev); assert_eq!(flat.len(), 2);`.

- [ ] **Step 5: Run the jobs tests to verify all pass**

Run: `cargo test`
Expected: PASS across the whole crate. `View` is untouched in this task, so the crate still compiles; the only cross-file change is the one-line `JobsUpdate` literal fix in `app.rs`'s test (Step 3 item 9). `JobsUpdate.hosted` is populated but not yet read by the app — harmless.

- [ ] **Step 6: Commit**

```bash
git add src/jobs.rs src/app.rs
git commit -m "feat: poll queued runs + collect hosted jobs with per-scope preservation"
```

---

### Task 4: Wire hosted through View, app, and UI

Adding the borrow field `View.hosted` forces updating **every** `View { .. }` literal in the same compile unit at once (`cargo test` compiles the lib tests *and* `examples/screenshot.rs`). So the `View` field, the `AppState` wiring, the renderer, and all literals land in one commit that compiles green. This is the natural atomic unit — a reviewer cannot accept `View.hosted` without every literal updated.

**Files:**
- Modify: `src/ui.rs` (`View.hosted`, `HOSTED_CAP`, `hosted_height`, `fmt_wait`, `render_hosted`, `render` layout; update all 8 `View { .. }` test literals)
- Modify: `src/app.rs` (`AppState.hosted`, `apply_jobs_update`, `draw` passes `&state.hosted`)
- Modify: `examples/screenshot.rs` (demo hosted rows, `View.hosted`, grow `ROWS`)

**Interfaces:**
- Consumes: `HostedJob`, `HostedStatus` (Task 1), `JobsUpdate.hosted` (Task 3); existing `fmt_elapsed`, `elapsed_secs`, `truncate_ellipsis`, `Palette`.
- Produces: `View` gains `pub hosted: &'a [HostedJob]`; `AppState` gains `hosted: Vec<HostedJob>`; `render` shows the section when non-empty.

- [ ] **Step 1: Write the failing tests (UI helpers + app wiring)**

Add to the `tests` module in `src/ui.rs`:

```rust
#[test]
fn hosted_height_is_zero_when_empty_and_caps_with_overflow() {
    assert_eq!(hosted_height(0), 0);
    assert_eq!(hosted_height(3), 1 + 3); // header + 3 rows
    assert_eq!(hosted_height(HOSTED_CAP), 1 + HOSTED_CAP as u16);
    // over cap → header + CAP rows + one "+N more" line
    assert_eq!(hosted_height(HOSTED_CAP + 5), 1 + HOSTED_CAP as u16 + 1);
}

#[test]
fn fmt_wait_compact_units() {
    assert_eq!(fmt_wait(8), "8s");
    assert_eq!(fmt_wait(125), "2m");
    assert_eq!(fmt_wait(3720), "1h2m");
}
```

Add to the `tests` module in `src/app.rs` (and add `HostedJob, HostedStatus` to its `use crate::model::{..}` line):

```rust
#[test]
fn jobs_update_sets_hosted() {
    let mut state = AppState::default();
    let hosted = vec![HostedJob {
        workflow: "CI".into(),
        job: "Build".into(),
        label: "ubuntu-latest".into(),
        branch: "main".into(),
        status: HostedStatus::InProgress,
        since: SystemTime::now(),
    }];
    apply_jobs_update(
        &mut state,
        JobsUpdate {
            jobs: HashMap::new(),
            hosted,
            error: None,
        },
    );
    assert_eq!(state.hosted.len(), 1);
    assert_eq!(state.hosted[0].job, "Build");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib ui::tests::hosted_height_is_zero_when_empty_and_caps_with_overflow ui::tests::fmt_wait_compact_units app::tests::jobs_update_sets_hosted`
Expected: FAIL — `cannot find function hosted_height` / `fmt_wait` / `HOSTED_CAP`; `AppState` has no field `hosted`.

- [ ] **Step 3a: Implement the UI section**

In `src/ui.rs`:

1. Add the field to `View`:

```rust
    pub crit_ratio: f64,
    /// Hosted (GitHub-hosted) jobs — running + queued — shown in their own
    /// section below the runner table. Empty ⇒ section hidden.
    pub hosted: &'a [HostedJob],
```

Add `HostedJob, HostedStatus` to the `use crate::model::{..}` import.

2. Add constants + helpers (near the other `const`s / `fmt_*`):

```rust
/// Max hosted rows rendered before collapsing the rest into a `+N more` line.
const HOSTED_CAP: usize = 6;

/// Vertical cells the hosted section needs for `n` jobs: 0 when empty, else a
/// header row + up to `HOSTED_CAP` job rows + one overflow line when truncated.
fn hosted_height(n: usize) -> u16 {
    if n == 0 {
        return 0;
    }
    let shown = n.min(HOSTED_CAP) as u16;
    let overflow = if n > HOSTED_CAP { 1 } else { 0 };
    1 + shown + overflow
}

/// Compact wait/elapsed for queued jobs: `8s`, `2m`, `1h2m`.
fn fmt_wait(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}
```

3. Add the renderer:

```rust
const HOSTED_LABEL_W: u16 = 14;
const HOSTED_ELAPSED_W: u16 = 12;

/// Column rects for the hosted table: `workflow › job` (flex), `label`,
/// `branch`, `elapsed` — mirrors `column_layout`'s read-back approach so the
/// flexing cells are truncated to exactly what ratatui allocates.
fn hosted_col_layout(area: Rect) -> [Rect; 4] {
    Layout::horizontal([
        Constraint::Min(12),
        Constraint::Length(HOSTED_LABEL_W),
        Constraint::Min(8),
        Constraint::Length(HOSTED_ELAPSED_W),
    ])
    .flex(Flex::Start)
    .spacing(COL_SPACING)
    .areas(area)
}

fn hosted_row(j: &HostedJob, now: SystemTime, p: &Palette, job_w: usize, branch_w: usize) -> Row<'static> {
    let (glyph, color) = match j.status {
        HostedStatus::InProgress => ('\u{25cf}', p.busy), // ●
        HostedStatus::Queued => ('\u{25cb}', p.warn),     // ○
    };
    let wj = format!("{} {} \u{203a} {}", glyph, j.workflow, j.job);
    let branch = if j.branch.is_empty() {
        "-".to_string()
    } else {
        j.branch.clone()
    };
    let elapsed = match j.status {
        HostedStatus::InProgress => fmt_elapsed(elapsed_secs(j.since, now)),
        HostedStatus::Queued => format!("queued {}", fmt_wait(elapsed_secs(j.since, now))),
    };
    Row::new(vec![
        Cell::from(truncate_ellipsis(&wj, job_w)),
        Cell::from(truncate_ellipsis(&j.label, HOSTED_LABEL_W as usize)),
        Cell::from(truncate_ellipsis(&branch, branch_w)),
        Cell::from(elapsed),
    ])
    .style(Style::new().fg(color).bg(p.base))
}

fn render_hosted(frame: &mut Frame, area: Rect, view: &View) {
    let p = view.palette;
    let header = Row::new(vec!["hosted", "label", "branch", "elapsed"])
        .style(Style::new().fg(p.text).bg(p.base).bold());
    let cols = hosted_col_layout(area);
    let job_w = cols[0].width as usize;
    let branch_w = cols[2].width as usize;

    let n = view.hosted.len();
    let shown = n.min(HOSTED_CAP);
    let mut rows: Vec<Row> = view.hosted[..shown]
        .iter()
        .map(|j| hosted_row(j, view.now, p, job_w, branch_w))
        .collect();
    if n > HOSTED_CAP {
        rows.push(
            Row::new(vec![Cell::from(format!("+{} more", n - HOSTED_CAP))])
                .style(Style::new().fg(p.idle).bg(p.base)),
        );
    }
    let table = Table::new(
        rows,
        [
            Constraint::Min(12),
            Constraint::Length(HOSTED_LABEL_W),
            Constraint::Min(8),
            Constraint::Length(HOSTED_ELAPSED_W),
        ],
    )
    .header(header)
    .column_spacing(COL_SPACING)
    .style(Style::new().fg(p.text).bg(p.base));
    frame.render_widget(table, area);
}
```

4. Wire it into `render` — compute `hosted_h`, add its constraint before the gauge, and render it:

Replace the constraints-building block and the body/gauge indexing:

```rust
    let has_banner = view.status.is_some();
    let hosted_h = hosted_height(view.hosted.len());
    let mut constraints = vec![Constraint::Length(1)];
    if has_banner {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    if hosted_h > 0 {
        constraints.push(Constraint::Length(hosted_h));
    }
    constraints.push(Constraint::Length(1));
    let chunks = Layout::vertical(constraints).split(frame.area());
```

And after the body render (the `render_table` / `render_empty_state` block) and before the gauge:

```rust
    let body_area = chunks[idx];
    idx += 1;
    if view.rows.is_empty() {
        render_empty_state(frame, body_area, view);
    } else {
        render_table(frame, body_area, view);
    }

    if hosted_h > 0 {
        render_hosted(frame, chunks[idx], view);
        idx += 1;
    }

    let gauge_area = chunks[idx];
    render_gauge(frame, gauge_area, view);
```

5. Add `hosted: &[],` to every existing `View { .. }` literal in `src/ui.rs` tests (8 sites: lines ~450, 655, 806, 866, 909, 939, 1052, 1087 in the pre-change file — grep `View {` to find them).

- [ ] **Step 3b: Implement the app wiring**

In `src/app.rs`:

1. Add the field to `AppState` (after `jobs_err`):

```rust
    jobs_err: Option<String>,
    hosted: Vec<crate::model::HostedJob>,
    history: History,
```

2. In `apply_jobs_update`, set it:

```rust
fn apply_jobs_update(state: &mut AppState, update: JobsUpdate) {
    state.jobs_err = update.error;
    state.jobs = update.jobs;
    state.hosted = update.hosted;
}
```

3. In `draw`, add to the `View { .. }` literal:

```rust
                hosted: &state.hosted,
```

- [ ] **Step 3c: Implement the screenshot example**

In `examples/screenshot.rs`:

1. Extend the model import: `use pitwall::model::{mem_level, HostedJob, HostedStatus, JobInfo, Load, MemLevel, RunnerRow, SourceKind};`

2. Add a builder returning three hosted jobs (two running, one queued), anchored to the example's fixed `now` (`UNIX_EPOCH + 1_700_000_000`):

```rust
fn demo_hosted(now: SystemTime) -> Vec<HostedJob> {
    let ago = |s: u64| now - Duration::from_secs(s);
    vec![
        HostedJob {
            workflow: "Deploy".into(),
            job: "build".into(),
            label: "ubuntu-latest".into(),
            branch: "main".into(),
            status: HostedStatus::InProgress,
            since: ago(72),
        },
        HostedJob {
            workflow: "E2E".into(),
            job: "chromium".into(),
            label: "ubuntu-24.04".into(),
            branch: "main".into(),
            status: HostedStatus::InProgress,
            since: ago(44),
        },
        HostedJob {
            workflow: "Release".into(),
            job: "publish".into(),
            label: "ubuntu-latest".into(),
            branch: "v2.1".into(),
            status: HostedStatus::Queued,
            since: ago(8),
        },
    ]
}
```

3. Grow the grid so the section fits — the hosted block adds `hosted_height(3) = 4` cells:

```rust
const ROWS: u16 = 13; // title + header + 6 runner rows + gauge (9) + hosted (4)
```

4. Set it on the `View` (build `let hosted = demo_hosted(now);` before `term.draw`, then add the field):

```rust
                crit_ratio: CRIT,
                hosted: &hosted,
```

- [ ] **Step 4: Run the targeted tests to verify they pass**

Run: `cargo test --lib ui::tests::hosted_height_is_zero_when_empty_and_caps_with_overflow ui::tests::fmt_wait_compact_units app::tests::jobs_update_sets_hosted`
Expected: PASS.

- [ ] **Step 5: Full build + test (all targets, incl. the example)**

Run: `cargo build --examples && cargo test`
Expected: PASS across the crate — every `View`/`JobsUpdate` literal now carries `hosted`.

- [ ] **Step 6: Commit**

```bash
git add src/ui.rs src/app.rs examples/screenshot.rs
git commit -m "feat: render hosted jobs section + wire through app and screenshot"
```

---

### Task 5: README + regenerated screenshot

**Files:**
- Modify: `README.md` (add "Hosted jobs" subsection)
- Regenerate: `docs/pitwall.png` via `make screenshot` (best-effort — needs `rsvg-convert` + fonts on the host)

- [ ] **Step 1: Verify the example renders SVG**

Run: `cargo run --release --example screenshot > /tmp/pitwall.svg && head -c 64 /tmp/pitwall.svg`
Expected: compiles; output begins with `<svg` (or an XML/`<?xml` prolog) and the hosted rows are present in the buffer.

- [ ] **Step 2: Regenerate the PNG (best-effort)**

Run: `make screenshot`
Expected: writes `docs/pitwall.png`. If it fails because `rsvg-convert` or the fonts are missing in this environment, leave the PNG unchanged and note it in the PR description as "regenerate on host" — the screenshot *code* already landed in Task 4.

- [ ] **Step 3: Add the README subsection**

In `README.md`, after the "Native runners" section, add:

```markdown
## Hosted jobs

Below the runner table, pitwall lists **GitHub-hosted** jobs for the configured
repos — both running and queued. Hosted runners are ephemeral per-job VMs on
GitHub's infrastructure, so there is **no CPU/mem to show**: this section carries
only `workflow › job`, the requested runner label (`ubuntu-latest`, …), branch,
and elapsed (running) or wait time (`queued 8s`, from the job's creation).

- `●` a running hosted job; `○` a queued one (waiting for a hosted runner).
- Sourced from **repo** scopes only — the per-job endpoint is repo-scoped, so
  org-scoped entries contribute nothing here (same limitation as org busy status).
- A job is treated as self-hosted (and shown in the table above instead) when its
  `labels` include `self-hosted`; everything else is hosted.
- The section is hidden when there are no hosted jobs, and caps at a handful of
  rows with a `+N more` line so a large matrix can't crowd out the runner table.
```

- [ ] **Step 4: Commit**

```bash
git add README.md docs/pitwall.png
git commit -m "docs: hosted jobs README subsection + regenerated screenshot"
```

---

## Verification (after all tasks)

- [ ] `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` all clean.
- [ ] Run `pitwall` against the live box (repo scope set) and confirm: a hosted section appears when a hosted job runs, running rows show elapsed, queued rows show `queued Ns`, self-hosted runners still appear in the table above, and the section vanishes when no hosted jobs exist.
- [ ] Validate the `self-hosted`-label discriminator against a real payload: `gh api "repos/<repo>/actions/runs/<id>/jobs" | jq '.jobs[] | {name, labels, runner_name}'` — confirm self-hosted jobs carry `self-hosted` in `labels` and hosted ones don't.
```
