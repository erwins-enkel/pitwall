# Near-cap memory alerting — design

Issue: [#8](https://github.com/erwins-enkel/pitwall/issues/8). Follow-up from #1.

## Goal

Visually emphasise when memory approaches limits — the `pulse-ci.slice` total
nearing the 24 GiB cap, or an individual runner nearing its 8g container limit.
Add an early-warning tier *below* critical without swallowing the existing
healthy-busy signal. **Visual only**; desktop notifications are out of scope for
v1.

## Current state

- Per-runner near-cap already exists: `Load::NearCap` fires at `mem ≥ 90%` of the
  container limit (`model.rs:59`), painting the **whole row** red (`ui.rs`).
- The slice-total gauge has **no** near-cap emphasis — always cyan (`ui.rs`).
- No warn (early-warning) tier anywhere; rows are binary
  (idle / busy / near-cap).

## What changes

### Two separate visual channels (key decision)

A runner row states two independent facts, so they get two independent color
channels instead of one overloaded `Load` color:

- **Row color = job state** (unchanged): dim = idle, green = busy, red =
  near-cap/critical. Existing `Load` enum, kept verbatim.
- **Mem-cell color = memory pressure warn tier** (new): the `mem` cell *alone*
  turns **yellow** in the warn band `[warn, crit)`. The rest of the row keeps its
  job-state color. So a busy runner under mild memory pressure still reads green
  ("healthy, working") with a yellow mem cell ("watch memory") — the two signals
  don't clobber each other.

**Escalation model:** mild pressure → localized signal (yellow mem cell); severe
pressure (≥ crit) → global signal (whole row red). The whole-row red at critical
is deliberately retained: it is the *existing* near-cap behavior, a loud
row-level alert is appropriate at the cap, and keeping it preserves the current
regression. This is why there is **no `Load::Warn` variant** — folding a memory
tier into the job-state enum is exactly what would make warn override busy-green;
a separate cell channel avoids the conflation.

### Threshold model

| Ratio | Tier | Row | Mem cell | Gauge |
|---|---|---|---|---|
| `< warn` | Normal | dim/green (job state) | inherits row | cyan |
| `≥ warn`, `< crit` | Warn | dim/green (job state) | **yellow** | yellow |
| `≥ crit` | Critical | **red** (whole row) | red (inherits) | red |

**Shared thresholds, applied identically to both ratios — intended, not
incidental.** The same `warn`/`crit` ratios classify *both* the per-runner ratio
`used / mem_limit` (≈ /8g) *and* the slice-total ratio `total / slice_cap`
(≈ /24GiB): one notion of "fraction of a memory budget consumed", regardless of
which budget. So mem cells and the gauge always agree on what "near cap" means.

- Defaults: **warn 85%**, **crit 90%** — chosen up front. `crit = 90%` is today's
  per-runner rule, unchanged (regression guard). `warn = 85%` sits just under
  crit (5-point band) and high enough to avoid firing on routine busy load.
- Configurable via `PITWALL_MEM_WARN_PCT` / `PITWALL_MEM_CRIT_PCT` (integer
  percents, e.g. `85`, `90`).
- Parse rule: clamp each to `0..=100`; if `warn > crit`, pin `warn = crit` so
  state can never invert. Stored as fractions (`f64`).
- Degenerate `crit = 0` (which pins `warn = 0` → every ratio Critical/red) is
  **accepted as deliberate misconfiguration**, not guarded — an explicit "always
  alert" override.

### `model.rs`

- **`Load` is unchanged** — `Idle`, `Busy`, `NearCap`. No `Warn` variant.
- Add `MemLevel { Normal, Warn, Critical }` and a pure helper:

  ```rust
  pub fn mem_level(used: u64, limit: u64, warn_ratio: f64, crit_ratio: f64) -> MemLevel
  ```

  Returns `Normal` when `limit == 0` (divide-by-zero guard, matching the existing
  `mem_limit > 0` check). Single source of truth for both mem cells and gauge.
- `RunnerRow` gains a `mem_level: MemLevel` field.
- `join` gains `warn_ratio`/`crit_ratio`. Per row it computes `mem_level`, stores
  it, and derives `Load` from it:
  - `Critical → Load::NearCap`
  - else `Busy` if a job is joined, else `Idle`

  A busy runner in the warn band therefore stays `Load::Busy` (green row); only
  its `mem_level` is `Warn`, which the UI renders as a yellow mem cell.

### `config.rs`

- Add `warn_ratio: f64`, `crit_ratio: f64` to `Config`.
- Factor the parse into a **pure helper** that takes the raw env values as
  arguments (no env access inside):

  ```rust
  fn resolve_thresholds(warn_raw: Option<&str>, crit_raw: Option<&str>) -> (f64, f64)
  ```

  Applies defaults `85`/`90`, converts percent→fraction, clamps `0..=100`, pins
  `warn = crit` when `warn > crit`. `Config::from_env` reads
  `PITWALL_MEM_WARN_PCT` / `PITWALL_MEM_CRIT_PCT` and delegates. Pure logic → unit
  test with plain arguments, no process-env mutation, no parallel-test flakiness.

### `ui.rs`

- **`load_style` is unchanged** (no `Warn` arm).
- `table_row`: when the row's `mem_level == MemLevel::Warn`, set the `mem`
  `Cell`'s own style to `fg = Yellow` (overrides the row style for that one cell).
  Otherwise the cell inherits the row style — so at critical the mem cell is red
  along with the rest of the row.
- Gauge: classify the slice ratio through `mem_level` → cyan (Normal) / yellow
  (Warn) / red (Critical). Append a **text marker** (accessibility + the issue's
  "visual emphasis"), using the codebase's `\u{...}` convention (cf. existing
  `\u{203a}`/`\u{2014}`/`\u{2026}`), `\u{26a0}` for the warning sign:
  - normal: `"{used} / {cap} GiB"` (unchanged)
  - warn: append ` \u{26a0} warn`
  - critical: append ` \u{26a0} NEAR CAP`
- `View` gains `warn_ratio`/`crit_ratio` so the gauge classifies via the same
  `mem_level` helper.

### `app.rs`

- Thread `cfg.warn_ratio` / `cfg.crit_ratio` into `join` and into `View`.
- No event-loop, timer, or channel changes. Classification is purely render-time.

### Signature-change fan-out

Changing `join`'s signature touches its only call site and its existing tests —
all updated together so the build stays green:

- `src/app.rs:105` — sole `join(...)` call site; pass the two threshold args.
- `src/model.rs` — the four existing `join(...)` tests (`no_job_is_idle`,
  `job_present_is_busy`, `high_mem_is_near_cap`,
  `rows_sorted_by_index_and_slice_summed`) each gain the two args, passed defaults
  `0.85, 0.90` so assertions stay unchanged.
- Regression guard: default `crit = 0.90` reproduces the current hardcoded
  `>= 0.9` rule at `model.rs:59`; the old literal is removed without shifting the
  critical threshold.

### Docs (`README.md`)

- Note the yellow mem-cell warn tier and the whole-row red at near-cap.
- Add `PITWALL_MEM_WARN_PCT` and `PITWALL_MEM_CRIT_PCT` to the config table.

## Non-goals (v1)

- Desktop notifications (libnotify / `notify-rust`).
- Edge-trigger / anti-spam / hysteresis logic (no notifications ⇒ not needed).
- Title-bar or banner alert summary/count.

## Testing

Follows the existing unit-test patterns.

- `model.rs`: `mem_level` boundaries — `< warn` = `Normal`, `[warn, crit)` =
  `Warn`, `>= crit` = `Critical`, `limit == 0` = `Normal`. `join`: **a busy
  runner (job present) at 0.87 → `Load::Busy` with `mem_level == Warn`** (proves
  busy-green is preserved); existing idle/busy/near-cap tests still pass.
- `config.rs`: test the pure `resolve_thresholds` helper directly with plain
  arguments (no `set_var`/`remove_var`) — defaults (`None`/`None` → 0.85/0.90);
  clamp both ends (>100, and non-numeric → default); `warn > crit` pins
  `warn = crit`; degenerate `crit = 0` → `(0.0, 0.0)`.
- `ui.rs`: via `TestBackend` — **marker text**: the gauge renders ` \u{26a0} warn`
  in the warn band, ` \u{26a0} NEAR CAP` at critical, neither when normal.
  **Actual color** (primary deliverable, asserted on buffer cell `.fg`/`.style`):
  a warn-band busy row keeps green cells but its `mem` cell is `Color::Yellow`; a
  critical row is red; the gauge cells are yellow at warn and red at critical.

## Success criteria

- A busy runner in `[warn, crit)` keeps a **green row** with a **yellow `mem`
  cell** — the healthy-busy signal is not swallowed.
- A runner `>= crit` shows a **red whole row**, exactly as today (no regression).
- Gauge: cyan `< warn`, yellow `>= warn`, red `>= crit`, with matching text marker.
- The same warn/crit ratios drive both per-runner mem cells and the slice gauge.
- Thresholds configurable via the two env vars; misconfiguration can't invert.
- `cargo test`, `cargo clippy`, `cargo fmt --check` all clean.
