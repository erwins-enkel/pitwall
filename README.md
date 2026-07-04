# pitwall

btop-like terminal UI for self-hosted GitHub Actions runners: live CPU/mem per runner container, joined with the workflow › job currently running.

## What you see

A table, one row per runner:

| runner | cpu | ~cpu | mem | ~mem | workflow › job | elapsed |
|---|---|---|---|---|---|---|

- **runner** — container name, e.g. `ci-runner-1`.
- **cpu / mem** — live usage from rootless docker (mem shown as `used/limit`).
- **~cpu / ~mem** — block-char sparklines of the last ~40s (20 samples at the 2s
  poll). CPU auto-scales to its window max with a 10% floor, so idle jitter reads
  as a flat baseline; mem scales to the container limit. History is in-memory and
  resets on restart.
- **workflow › job** — `— idle` when no in-progress job is joined, else `Workflow Name › Job Name`.
- **elapsed** — ticking duration since the job started; `-` when idle.

Rows are colored by load using the Catppuccin palette over a full Catppuccin background: muted gray = idle, green = busy, red = near-cap (mem ≥ crit % of the container limit — the whole row goes red). As an early warning *below* near-cap, the `mem` cell alone turns the warn color (yellow) while its memory is in the warn band (≥ warn %, < crit %), so a busy runner stays green with just a yellow mem cell. The gauge at the bottom shows summed runner memory against a configurable slice cap, using the same thresholds: teal normally, yellow (` ⚠ warn`) in the warn band, red (` ⚠ NEAR CAP`) at the cap. Pick a flavor with `PITWALL_THEME` (see [Configuration](#configuration)). Colors assume a truecolor terminal; on 16/256-color terminals they downsample to the nearest available color.

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

Every setting is optional and resolved in this order (first wins): **env var →
config file → built-in default**.

| Setting | Env var | File key | Default |
|---|---|---|---|
| socket | `PITWALL_SOCKET` | `socket` | `$DOCKER_HOST` (with `unix://` stripped) if set, else `/run/user/$UID/docker.sock` |
| repo | `PITWALL_REPO` | `repo` | `owner/repo` (set this to your runners' repo) |
| prefix | `PITWALL_PREFIX` | `prefix` | `ci-runner-` (must match your runner container names, e.g. `pulse-ci-runner-`) |
| slice cap (GiB) | `PITWALL_SLICE_CAP_GIB` | `slice_cap_gib` | `24` |
| theme | `PITWALL_THEME` | `theme` | `mocha` — Catppuccin flavor: `mocha`, `macchiato`, `frappe`, or `latte` (light). Unknown values fall back to `mocha`. |
| mem warn % | `PITWALL_MEM_WARN_PCT` | — | `85` (warn tier: yellow mem cell / gauge) |
| mem crit % | `PITWALL_MEM_CRIT_PCT` | — | `90` (critical tier: red row / gauge) |

An empty env var (e.g. `PITWALL_REPO=`) is treated as unset, falling through to
the file value, then the default.

Both memory percents are clamped to `0..=100`; if warn exceeds crit it is pinned
down to crit so the tiers can't invert.

### Config file

An optional TOML file provides persistent settings. It is read from
`$XDG_CONFIG_HOME/pitwall/config.toml` (falling back to
`~/.config/pitwall/config.toml`); set `PITWALL_CONFIG=/path/to/config.toml` to
point elsewhere. All keys are optional:

```toml
# ~/.config/pitwall/config.toml
socket        = "/run/user/1000/docker.sock"
repo          = "owner/repo"
prefix        = "ci-runner-"
slice_cap_gib = 24
theme         = "mocha"
```

Notes:

- **`socket` beats `DOCKER_HOST`.** The full socket order is `PITWALL_SOCKET` →
  file `socket` → `DOCKER_HOST` → `/run/user/$UID/docker.sock`. The file value
  intentionally overrides the ambient `DOCKER_HOST` env var, since it is
  deliberate pitwall configuration rather than an ambient docker setting.
- A malformed file, or one with an unknown key, is a hard error: pitwall reports
  it and exits without starting the UI.
- The default file is optional — its absence is fine. A `PITWALL_CONFIG` path
  that is set but missing is an error.

## How it works

- **Resources** — polled every ~2s via `bollard` against the rootless docker socket. Docker's stats API is one-shot (no streaming), so pitwall retains the previous poll's CPU counters per container id and computes CPU% as a delta between samples.
- **Jobs** — polled every ~15s via `gh`. Only in-progress jobs on self-hosted runners are considered; GitHub-hosted, queued, and completed jobs are excluded.
- **Join** — by trailing runner index: container `ci-runner-N` matches GitHub runner name `runner-N`.
- **Gotcha** — a running container with no in-progress job is normal idle state, not an error. The runners are ephemeral and deregister between jobs, so gaps between "container up" and "job assigned" are expected.
- **Degradation** — a broken docker socket or `gh` failure surfaces as a red status banner and keeps the last-known-good data on screen instead of blanking the UI; the empty state is self-diagnosing (`waiting for runners…`, `waiting for runner stats…`, or `N containers running, none match prefix '…'`). No source failure panics; `q`/`Esc`/`Ctrl-C` always restore the terminal.
