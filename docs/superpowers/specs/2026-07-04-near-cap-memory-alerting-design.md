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
- Degenerate `crit = 0` (which pins `warn = 0` → every ratio Critical/red) is
  **accepted as deliberate misconfiguration**, not guarded: setting crit to 0 is
  an explicit "always alert" override.

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

  **Precedence — memory pressure overrides job state** (as today for NearCap, now
  extended to Warn): a runner with a live job whose memory is in the 75–90% band
  renders `Load::Warn` (yellow), *not* `Busy` (green). This is deliberate — the
  operator should see pressure regardless of whether a job is currently joined
  (jobs poll only every ~15s and ephemeral runners deregister between jobs).

### `config.rs`

- Add `warn_ratio: f64`, `crit_ratio: f64` to `Config`.
- Factor the parse into a **pure helper** that takes the raw env values as
  arguments (no env access inside):

  ```rust
  fn resolve_thresholds(warn_raw: Option<&str>, crit_raw: Option<&str>) -> (f64, f64)
  ```

  It applies defaults `75`/`90`, converts percent→fraction, clamps `0..=100`, and
  pins `warn = crit` when `warn > crit`. `Config::from_env` reads
  `PITWALL_MEM_WARN_PCT` / `PITWALL_MEM_CRIT_PCT` and delegates to it. Keeping the
  logic pure lets it be unit-tested with plain arguments — no process-env
  mutation, hence no flakiness when config tests run in parallel.

### `ui.rs`

- `load_style`: add `Load::Warn => Yellow`. Idle / Busy / NearCap unchanged.
- Gauge: classify the slice ratio through `mem_level` → cyan (normal) /
  yellow (warn) / red (critical). Append a **text marker** so the signal is not
  color-only (accessibility + the issue's "visual emphasis"). Use the codebase's
  `\u{...}` escape convention (as with existing `\u{203a}`/`\u{2014}`/`\u{2026}`),
  i.e. `\u{26a0}` for the warning sign:
  - normal: `"{used} / {cap} GiB"` (unchanged)
  - warn: append ` \u{26a0} warn`
  - critical: append ` \u{26a0} NEAR CAP`
- `View` carries `warn_ratio`/`crit_ratio` so the gauge classifies through the
  same `mem_level` helper as the rows.

### `app.rs`

- Thread `cfg.warn_ratio` / `cfg.crit_ratio` into `join` and into `View`.
- No event-loop, timer, or channel changes. Classification is purely at render
  time.

### Signature-change fan-out

Changing `join`'s signature touches its only call site and its existing tests —
all updated together so the build stays green:

- `src/app.rs:105` — sole `join(...)` call site; pass the two threshold args.
- `src/model.rs` — the four existing `join(...)` tests (`no_job_is_idle`,
  `job_present_is_busy`, `high_mem_is_near_cap`,
  `rows_sorted_by_index_and_slice_summed`) each gain the two args, passed the
  defaults `0.75, 0.90` so assertions stay unchanged.
- Regression guard: default `crit = 0.90` reproduces the current hardcoded
  `>= 0.9` rule at `model.rs:59`; the old literal is removed without shifting the
  critical threshold.

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
  band yields `Load::Warn`; **a busy runner (job present) in the warn band yields
  `Load::Warn`, not `Busy`** (precedence); existing idle/busy/near-cap tests still
  pass.
- `config.rs`: test the pure `resolve_thresholds` helper directly with plain
  arguments (no `set_var`/`remove_var`) — defaults (`None`/`None` → 0.75/0.90);
  clamp both ends (>100, and non-numeric → default); `warn > crit` pins
  `warn = crit`; degenerate `crit = 0` → `(0.0, 0.0)`.
- `ui.rs`: via `TestBackend` — **marker text**: a slice ratio in the warn band
  renders ` \u{26a0} warn`, critical renders ` \u{26a0} NEAR CAP`, normal renders
  neither. **Actual color** (the primary deliverable, asserted on buffer cell
  `.fg`/`.style`, not just text): a `Load::Warn` row cell is `Color::Yellow`; the
  gauge cells are yellow in the warn band and red in the critical band.

## Success criteria

- Slice gauge turns yellow ≥ warn, red ≥ crit, with the matching text marker.
- Runner rows turn yellow ≥ warn (new), red ≥ crit (unchanged threshold).
- Thresholds configurable via the two env vars; misconfiguration can't invert.
- Live check acted upon: if warn=75% fires near-continuously on normally-busy CI
  runners (making yellow the de-facto busy color), the default warn is raised to
  sit just above observed steady-state busy memory (candidate 85%, ≥5-point band
  below crit); observed value + chosen default recorded in the PR.
- `cargo test`, `cargo clippy`, `cargo fmt --check` all clean.
