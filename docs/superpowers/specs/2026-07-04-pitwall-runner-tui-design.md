# pitwall — runner stats TUI (design)

Date: 2026-07-04
Status: approved (design), pending implementation plan

## Goal

A btop-like terminal UI showing, per self-hosted GitHub Actions runner, live
CPU/mem consumption joined with the repo/workflow/job currently running (if any).
v1 covers the 6 ephemeral docker "pulse" runners on a single self-hosted CI host.

## Decisions (settled)

- **Language/stack:** Rust + ratatui + crossterm, async on tokio. Single static
  binary installed to `~/.local/bin/pitwall`.
- **Docker stats:** `bollard`, streaming over the rootless socket
  (`unix:///run/user/1000/docker.sock`). No polling subprocess; CPU%/mem computed
  from consecutive cgroup samples.
- **Jobs:** shell out to the host-authenticated `gh` CLI (no in-Rust GitHub auth).
- **Runner scope (v1):** pulse docker runners only, single repo. A resource-source
  abstraction is carried so native (non-docker) runners can be added later without
  reworking the model/UI.
- **No bash prototype:** both data sources already validated live; go straight to Rust.

## Architecture

```
 ┌─ resource source (bollard, streaming) ─┐
 │   pulse-ci-runner-* → cpu%, mem        ├─► AppState ─► ui (table + slice gauge)
 ┌─ jobs source (gh shell-out, 15s) ──────┘        ▲
 │   in_progress runs → runner_name→job            └── key events (q quit)
```

Two independent background tasks own all I/O and push updates into a shared
`AppState`; a render loop only reads it. Communication via an mpsc event channel
(`Tick | Key | Resources | Jobs`).

## Components (each independently testable)

1. **`resource`** — bollard connects to the rootless docker socket, streams stats
   for containers whose name starts with `pulse-ci-runner-`. Computes CPU% and mem
   usage/limit from consecutive samples. Emits
   `RunnerResource { name, cpu_pct, mem_bytes, mem_limit }`.
   This module is the abstraction seam: it exposes a source trait/enum so a future
   `proc` source (native `Runner.Listener` processes) is purely additive. v1 ships
   only the docker implementation.
2. **`jobs`** — every 15s: `gh api repos/{repo}/actions/runs?status=in_progress`,
   then per run `…/runs/{id}/jobs`, mapping each job's `runner_name` ("runner-N") to
   `JobInfo { workflow, job, started_at }`. Retains last-known values on failure;
   never panics on rate limits or transient errors.
3. **`model`** — pure join, no I/O:
   `(Vec<RunnerResource>, Map<name, JobInfo>) → Vec<RunnerRow>`. Classifies
   idle/busy/near-cap, computes elapsed from `started_at`, sums slice memory vs the
   24 GiB cap. Fully unit-tested.
4. **`ui`** — renders the table `runner | CPU | mem | workflow › job | elapsed` plus
   the slice-total gauge; colors idle/busy/near-cap. A ratatui `TestBackend` render
   smoke test guards layout.
5. **`app` / `main`** — mpsc event loop; terminal raw-mode setup/teardown; panic hook
   restores the terminal before exit.

## Data flow & cadence

- Docker stats: continuous stream (~1 sample/s per container).
- Jobs: polled every 15s (rate-limit safe).
- UI: redraw on tick (~1s) or when new data arrives.

## Naming / join contract

Container `pulse-ci-runner-N` ↔ GitHub `runner_name` `runner-N`. The join keys on
the trailing integer N. A runner with a container but no in-progress job is **idle**
(the expected steady state for ephemeral runners), not an error.

## Error handling

- Docker socket unavailable → error banner, keep retrying, do not exit.
- `gh` failure / rate limit → retain last jobs, mark stale, carry on.
- Container up + no GitHub job → rendered as **idle** (never an error).
- Panic hook restores the terminal (cooked mode, alternate screen off) before exit.

## Config

Constants with env overrides — `PITWALL_REPO` (default `owner/repo`),
container prefix (default `pulse-ci-runner-`), slice cap (default 24 GiB). No config
file in v1.

## Testing (TDD)

- `model` join: idle/busy/near-cap classification, elapsed calc, slice-total sum.
- CPU%-delta math from sample cgroup readings.
- `gh` JSON parse against a captured fixture.
- One `TestBackend` render smoke test.
- Live end-to-end verification against the running runners before "done": trigger a
  real pulse CI job and confirm it appears joined to the right container.

## Install

`cargo build --release` → copy binary to `~/.local/bin/pitwall`. A `just`/make
target or short README section documents it.

## Non-goals (v1)

Native/non-pulse runners, historical graphs/sparklines, config file, alerting.
