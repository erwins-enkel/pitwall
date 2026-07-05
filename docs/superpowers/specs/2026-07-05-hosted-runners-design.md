# Hosted-runner status section for pitwall

## Goal

Extend pitwall to also show the status of **GitHub-hosted** runners for the
configured repos — with deliberately less detail than self-hosted runners,
because hosted runners are ephemeral per-job VMs on GitHub's infrastructure with
**no obtainable CPU/mem**. Surface the hosted *jobs* currently running and
queued: `workflow › job`, the requested runner label, branch, and elapsed /
wait time.

## Decision provenance

Task prompt: *"could we potentially extend pitwall to also show status of hosted
runners albeit with less detail?"* The specifics below were settled by explicit
clarifying questions answered during brainstorming (question → chosen answer):

- *"What should the hosted section include?"* → **In-progress + queued jobs**
- *"Where should hosted jobs appear in the TUI?"* → **Separate section below the table**
- *"Always on, or opt-in?"* → **Always on** (shown when hosted jobs exist, hidden when empty)
- *"Show queued wait time, or a static `queued` label?"* → **Show wait time**

## Core constraint

GitHub-hosted runners are not persistent, enumerable entities and expose no
resource telemetry. "Less detail" is therefore not a choice but the ceiling: we
can only show job-level facts GitHub's API returns. Hosted entries are **jobs**,
not runners, so they get their own section with no resource columns — the main
table's cpu/~cpu/mem/~mem columns are meaningless for them.

## Decisions

- **What shows:** hosted jobs in status `in_progress` or `queued` for the
  configured **repo** scopes. Org-scoped entries are skipped — the per-job
  endpoint is repo-scoped, the same documented limitation as org busy status.
- **Discriminator:** a job is self-hosted iff its `labels` array contains
  `"self-hosted"` (GitHub auto-adds this label to every self-hosted runner job;
  hosted jobs never carry it). Everything else is hosted. This is the one
  assumption to validate against a live payload during implementation.
- **Placement:** a distinct `hosted` section between the runner table and the
  bottom gauge. Shown only when the hosted list is non-empty.
- **Always on:** no config flag. Zero new settings.
- **Queued wait time:** shown (e.g. `queued 8s`), computed from the job's
  `created_at`. Running jobs show elapsed from `started_at`.
- **Self-hosted path unchanged:** the existing `RunnerKey`/`join` flow is
  untouched. Self-hosted queued jobs are out of scope (not surfaced).
- **Overflow:** the section is capped (~6 rows) with a `+N more` line so a large
  matrix can't crowd out the runner table.

## Data layer (`src/jobs.rs`)

The poller already fetches `runs?status=in_progress` → jobs per repo. Changes:

- **Also poll `runs?status=queued`** per repo. Union the run IDs from both
  status queries, fetch each run's jobs once.
- **Classify each job** by the `self-hosted` label:
  - self-hosted → existing `RunnerKey` slice (unchanged: in-progress only).
  - hosted, status ∈ {`queued`, `in_progress`} → new `HostedJob`.
- `parse_jobs` is extended (or paired with a `parse_hosted_jobs`) to return
  hosted jobs alongside the self-hosted `(runner_name, JobInfo)` pairs. Job
  fields used: `name` (job), `labels` (→ representative label; the first
  non-`self-hosted` label, e.g. `ubuntu-latest`), `status`, `started_at`,
  `created_at`. Workflow name and branch come from the run, as today.
- **Preservation:** hosted jobs get the same per-scope last-known-good treatment
  as runner slices — a per-scope `Vec<HostedJob>` kept on a failed poll,
  replaced on success. Mirrors `merge_scopes` / the existing `Slice` handling.

### `JobsUpdate`

```rust
pub struct JobsUpdate {
    pub jobs: Slice,               // unchanged: self-hosted RunnerKey → job
    pub hosted: Vec<HostedJob>,    // new: hosted jobs across all repo scopes
    pub error: Option<String>,
}
```

## Model (`src/model.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedStatus { InProgress, Queued }

#[derive(Debug, Clone)]
pub struct HostedJob {
    pub workflow: String,
    pub job: String,
    pub label: String,        // requested runner label, e.g. "ubuntu-latest"
    pub branch: String,
    pub status: HostedStatus,
    pub since: SystemTime,    // started_at (running) or created_at (queued)
}
```

- `join()` is untouched; hosted jobs bypass the runner join entirely.
- **Sort:** running first (longest elapsed first), then queued (longest wait
  first). A small helper `sort_hosted(&mut Vec<HostedJob>, now)` with a unit test.
- Reuse the existing `elapsed_secs(since, now)` for both elapsed and wait.

## UI (`src/ui.rs`)

- New `render_hosted(frame, area, view)`.
- **Layout:** in `render`, when `view.hosted` is non-empty, insert a
  `Constraint::Length(hosted_h)` between the body (`Min(1)`) and the gauge
  (`Length(1)`), where `hosted_h = 1 (header) + min(rows, CAP) + overflow?`.
  When empty, no constraint is added — layout is identical to today.
- **Columns** (no resource columns): `status glyph | workflow › job | label |
  branch | elapsed/wait`.
  - `●` running (busy/green), `○` queued (muted/yellow).
  - Running row → `1m12s`; queued row → `queued 8s`.
- **Overflow:** if `rows > CAP`, render `CAP` rows then a `+N more` line in the
  muted color.
- Colors from the existing `Palette` (accent/green for running, an existing
  muted/yellow role for queued). No new palette entries.

### `View`

Gains `hosted: &'a [HostedJob]`. `app.rs` stores the hosted list from
`JobsUpdate` and passes it into `View` in `draw`.

## App wiring (`src/app.rs`)

- `AppState` gains `hosted: Vec<HostedJob>`.
- `apply_jobs_update` sets `state.hosted = update.hosted` (preservation already
  handled in the poller, matching how `jobs` is mirrored).
- `draw` passes `&state.hosted` into `View`.

## Config

None. Always-on; active only for repo-scoped config (org scopes skipped).

## Documentation

README gets a short "Hosted jobs" subsection: what the section shows, the
no-resource-telemetry reason, repo-scope-only limitation, and the running/queued
glyphs. The dummy-data screenshot generator (`make screenshot`) gains a couple
of representative hosted rows so the screenshot reflects the feature.

## Testing

- `parse_hosted_jobs` / extended `parse_jobs`: a fixture with mixed jobs —
  self-hosted (has `self-hosted` label), hosted in_progress, hosted queued,
  completed (dropped). Assert hosted extraction, label pick, status, and that
  self-hosted classification is unchanged.
- Per-scope hosted preservation on a failed poll (parallel to the existing
  runner-slice preservation test).
- `sort_hosted`: running-before-queued, longest-first within each.
- UI: a snapshot/width test for the hosted section including the `+N more`
  overflow path (following the existing `column_layout` test style).

## Rate limits

Adds one `runs?status=queued` list call plus one jobs call per queued run, per
repo, per 15s poll. For a handful of repos this stays well within the 5000/hr
authenticated `gh` budget.

## Out of scope

- Self-hosted queued jobs (main table stays running-only).
- Hosted job resource telemetry (impossible).
- Org-scoped hosted job enumeration (no cheap endpoint).
- Scrolling/pagination beyond the `+N more` cap.
