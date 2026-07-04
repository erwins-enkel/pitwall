# Near-cap memory alerting — design

Issue: [#8](https://github.com/erwins-enkel/pitwall/issues/8). Follow-up from #1.

## Goal

Visually emphasise when memory approaches limits — the `pulse-ci.slice` total
nearing the 24 GiB cap, or an individual runner nearing its 8g container limit.
Two-tier (warn → critical). **Visual only**; desktop notifications are out of
scope for v1.

## Current state

- Per-runner near-cap already exists: `Load::NearCap` fires at `mem ≥ 90%` of the
  container limit (`model.rs`), painting the row red (`ui.rs`).
- The slice-total gauge has **no** near-cap emphasis — always cyan (`ui.rs`).
- No warn (early-warning) tier anywhere; rows are binary
  (idle / busy / near-cap).

## What changes

Introduce one shared notion of memory pressure with two thresholds, applied to
two ratios: each runner's `mem/limit` and the slice's `total/cap`. Both the
runner rows and the slice gauge classify through the **same** helper, so their
colors always agree.

### Threshold model

| Ratio | Tier | Color |
|---|---|---|
| `< warn` | normal | (rows: dim idle / green busy; gauge: cyan) |
| `≥ warn` and `< crit` | Warn | yellow |
| `≥ crit` | NearCap (critical) | red |

- Defaults: **warn 75%**, **crit 90%**. `crit = 90%` is today's per-runner rule,
  unchanged — no behavior regression at the critical tier.
- Configurable via `PITWALL_MEM_WARN_PCT` and `PITWALL_MEM_CRIT_PCT`
  (integer percents, e.g. `75`, `90`).
- Parse rule: clamp each to `0..=100`; if `warn > crit`, pin `warn = crit` so the
  state can never invert. Stored as fractions (`f64`, `0.0..=1.0`).

### `model.rs`

- Add `Load::Warn`. Variant order: `Idle`, `Busy`, `Warn`, `NearCap`.
- Add `MemLevel { Normal, Warn, Critical }` and a pure helper:

  ```rust
  pub fn mem_level(used: u64, limit: u64, warn_ratio: f64, crit_ratio: f64) -> MemLevel
  ```

  Returns `Normal` when `limit == 0` (guard against divide-by-zero, matching the
  existing `mem_limit > 0` check). This is the single source of truth.
- `join` gains `warn_ratio`/`crit_ratio` params. Per row:
  - `Critical → Load::NearCap`
  - `Warn → Load::Warn`
  - `Normal → Busy` if a job is joined, else `Idle`

  Memory pressure still overrides job state, exactly as today.

### `config.rs`

- Add `warn_ratio: f64`, `crit_ratio: f64` to `Config`.
- Parse `PITWALL_MEM_WARN_PCT` (default `75`) and `PITWALL_MEM_CRIT_PCT`
  (default `90`) with the clamp/pin rule above.

### `ui.rs`

- `load_style`: add `Load::Warn => Yellow`. Idle / Busy / NearCap unchanged.
- Gauge: classify the slice ratio through `mem_level` → cyan (normal) /
  yellow (warn) / red (critical). Append a **text marker** so the signal is not
  color-only (accessibility + the issue's "visual emphasis"):
  - normal: `"{used} / {cap} GiB"` (unchanged)
  - warn: append ` ⚠ warn`
  - critical: append ` ⚠ NEAR CAP`
- `View` carries `warn_ratio`/`crit_ratio` so the gauge classifies through the
  same `mem_level` helper as the rows.

### `app.rs`

- Thread `cfg.warn_ratio` / `cfg.crit_ratio` into `join` and into `View`.
- No event-loop, timer, or channel changes. Classification is purely at render
  time.

### Docs (`README.md`)

- Update the load-color sentence to include the yellow warn tier.
- Add `PITWALL_MEM_WARN_PCT` and `PITWALL_MEM_CRIT_PCT` to the config table.

## Non-goals (v1)

- Desktop notifications (libnotify / `notify-rust`).
- Edge-trigger / anti-spam / hysteresis logic (no notifications ⇒ not needed).
- Title-bar or banner alert summary/count.

## Testing

Follows the existing unit-test patterns.

- `model.rs`: `mem_level` boundaries — below warn = `Normal`, at warn = `Warn`,
  at crit = `Critical`, `limit == 0` = `Normal`. `join`: a runner in the warn
  band yields `Load::Warn`; existing idle/busy/near-cap tests still pass.
- `config.rs`: defaults (75/90 → 0.75/0.90); clamp out-of-range; `warn > crit`
  pins `warn = crit`.
- `ui.rs`: via `TestBackend`, a slice ratio in the warn band renders the ` ⚠ warn`
  marker; critical renders ` ⚠ NEAR CAP`; normal renders neither.

## Success criteria

- Slice gauge turns yellow ≥ warn, red ≥ crit, with the matching text marker.
- Runner rows turn yellow ≥ warn (new), red ≥ crit (unchanged threshold).
- Thresholds configurable via the two env vars; misconfiguration can't invert.
- `cargo test`, `cargo clippy`, `cargo fmt --check` all clean.
