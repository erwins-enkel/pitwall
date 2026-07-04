# pitwall

btop-like terminal UI for self-hosted GitHub Actions runners: live CPU/mem per runner container, joined with the workflow › job currently running.

## What you see

A table, one row per runner:

| runner | cpu | mem | workflow › job | elapsed |
|---|---|---|---|---|

- **runner** — container name, e.g. `ci-runner-1`.
- **cpu / mem** — live usage from rootless docker (mem shown as `used/limit`).
- **workflow › job** — `— idle` when no in-progress job is joined, else `Workflow Name › Job Name`.
- **elapsed** — ticking duration since the job started; `-` when idle.

Rows are colored by load: dim gray = idle, green = busy, red = near-cap (mem ≥ 90% of container limit). A gauge at the bottom shows summed runner memory against a configurable slice cap.

## Requirements

- Linux
- A reachable rootless Docker socket (default `/run/user/$UID/docker.sock`)
- An authenticated `gh` CLI (used for job data)

## Install

```sh
make install
```

Builds a release binary and installs it to `~/.local/bin/pitwall`. Make sure `~/.local/bin` is on your `PATH`.

Manual alternative:

```sh
cargo build --release
mkdir -p ~/.local/bin
cp target/release/pitwall ~/.local/bin/pitwall
```

## Usage

```sh
pitwall
```

Quit with `q`, `Esc`, or `Ctrl-C` — the terminal is restored on exit.

## Configuration

All settings are env vars, all optional:

| Var | Default |
|---|---|
| `PITWALL_SOCKET` | `$DOCKER_HOST` (with `unix://` stripped) if set, else `/run/user/$UID/docker.sock` |
| `PITWALL_REPO` | `owner/repo` (set this to your runners' repo) |
| `PITWALL_PREFIX` | `ci-runner-` |
| `PITWALL_SLICE_CAP_GIB` | `24` |

## How it works

- **Resources** — polled every ~2s via `bollard` against the rootless docker socket. Docker's stats API is one-shot (no streaming), so pitwall retains the previous poll's CPU counters per container id and computes CPU% as a delta between samples.
- **Jobs** — polled every ~15s via `gh`. Only in-progress jobs on self-hosted runners are considered; GitHub-hosted, queued, and completed jobs are excluded.
- **Join** — by trailing runner index: container `ci-runner-N` matches GitHub runner name `runner-N`.
- **Gotcha** — a running container with no in-progress job is normal idle state, not an error. The runners are ephemeral and deregister between jobs, so gaps between "container up" and "job assigned" are expected.
- **Degradation** — a broken docker socket or `gh` failure surfaces as a red status banner and keeps the last-known-good data on screen instead of blanking the UI; zero matching runners shows "waiting for runners…". No source failure panics; `q`/`Esc`/`Ctrl-C` always restore the terminal.
