# Catppuccin theming for pitwall

## Goal

Replace pitwall's hardcoded ANSI colors with the Catppuccin palette, selectable
across all four official flavors via an env var, and paint a full Catppuccin
background so the TUI looks consistent on any terminal.

## Decisions

- **Flavors:** all four — Mocha (default), Macchiato, Frappé, Latte — chosen via
  `PITWALL_THEME`.
- **Approach:** a semantic palette module (`theme.rs`); `ui.rs` references roles,
  not raw colors.
- **Background:** full Catppuccin `base` background + matching foreground.
- **Bad value:** unknown `PITWALL_THEME` silently falls back to Mocha, matching
  the lenient env parsing already in `config.rs`.
- No new dependencies (colors inlined as `Color::Rgb`), no runtime theme
  switching, no config file.
- **Full background is a deliberate tradeoff:** it overrides terminal
  background/transparency. Kept per explicit user choice (Latte covers light
  terminals). Not opt-in.
- **Truecolor assumption:** `Color::Rgb` downsamples to the nearest entry on
  16/256-color terminals — colors approximate, UI stays functional. Expected
  degradation, documented in the README.

## New module: `src/theme.rs`

```rust
#[derive(Clone, Copy)]
pub enum Flavor { Mocha, Macchiato, Frappe, Latte }

pub struct Palette {
    pub base: Color,      // full-screen background
    pub text: Color,      // default foreground: title fallback, table cells, header
    pub idle: Color,      // idle rows (rendered with DIM modifier, as today)
    pub busy: Color,      // busy rows
    pub near_cap: Color,  // near-cap rows
    pub gauge: Color,     // memory gauge fill
    pub error: Color,     // status banner
    pub accent: Color,    // "pitwall" title
}
```

- `Flavor::parse_lenient(&str) -> Flavor` — case-insensitive; matches
  `mocha`/`macchiato`/`frappe`/`latte`; anything else (typo, empty) → `Mocha`.
  `frappe` accepted without the accent. Named `parse_lenient` (not `from_str`)
  because parsing is infallible; an inherent `from_str(&str) -> Self` trips
  `clippy::should_implement_trait`. We do not implement `std::str::FromStr`.
- `Palette::for_flavor(Flavor) -> Palette` — maps each flavor's official hex to
  the roles.

### Role → Catppuccin color mapping

| role | Catppuccin name | replaces |
|---|---|---|
| `base` | base | (new — terminal bg) |
| `text` | text | default fg |
| `idle` | overlay0 | `DarkGray` + DIM |
| `busy` | green | `Green` |
| `near_cap` | red | `Red` |
| `gauge` | teal | `Cyan` |
| `error` | red | `Red` (banner) |
| `accent` | mauve | (new — title) |

Mocha reference hex (others sourced identically from the official palette during
implementation): base `#1e1e2e`, text `#cdd6f4`, overlay0 `#6c7086`,
green `#a6e3a1`, red `#f38ba8`, teal `#94e2d5`, mauve `#cba6f7`.

Official hex per flavor comes from https://catppuccin.com/palette — values will
be transcribed exactly for Mocha, Macchiato, Frappé, and Latte.

## Config

`src/config.rs`:
- `Config` gains `flavor: Flavor`.
- `from_env` reads `PITWALL_THEME` and parses via `Flavor::parse_lenient`
  (default Mocha when unset or unrecognized).

## Wiring

- `app::run` builds `let palette = Palette::for_flavor(cfg.flavor);` once, before
  the loop.
- `draw(...)` takes `&Palette` and stores it on the `View`.
- `ui::View` gains `pub palette: &'a Palette`.
- `ui::render(frame, view)` signature unchanged (palette read from `view`).

## Full background rendering (`ui.rs`)

1. First widget in `render`: a `Block` styled `Style::new().bg(base).fg(text)`
   over `frame.area()`, so gaps (column spacing, blank rows, unused lines) carry
   the base background.
2. Every subsequent widget's `Style` also carries `.bg(base)` so no drawn cell
   resets to the terminal default:
   - title `Paragraph`: `fg(accent).bg(base)`
   - banner `Paragraph`: `fg(error).bg(base).bold()`
   - header `Row`: `fg(text).bg(base).bold()`
   - table rows: `load_style` returns fg per load + `.bg(base)` (idle keeps DIM)
   - empty-state `Paragraph`: `fg(text).bg(base)`
   - gauge: overall `.bg(base)`; `gauge_style` fill `fg(gauge)`

`load_style` changes from a free function returning fixed colors to reading the
palette (`fn load_style(load: Load, p: &Palette) -> Style`).

## Testing

- `theme.rs`: `parse_lenient` tested **directly, with no env access** (so it never
  races the env-mutating `config.rs` tests under parallel execution) — `"mocha"`,
  `"MOCHA"`, `"latte"` map correctly; `"garbage"` and `""` → `Mocha`. Sanity check
  that distinct flavors yield distinct `base` colors.
- `ui.rs`: existing render tests updated to construct a
  `Palette::for_flavor(Flavor::Mocha)` and pass `&palette` in `View`. Content
  assertions (`ci-runner-1`, `idle`, `docker: unreachable`) still hold. **Add a
  test asserting a rendered buffer cell's `.bg` equals the Mocha `base` color**, so
  the full-background fill (the top risk) is regression-guarded, not just
  smoke-tested.
- `config.rs`: keeps its existing env tests; the flavor parser's correctness lives
  in the env-free `theme.rs` tests above.

## Docs

`README.md`:
- Add a `PITWALL_THEME` row (default `mocha`) to the env-var table and note the
  four accepted flavor values.
- Update the line-17 prose ("dim gray = idle, green = busy, red = near-cap") so the
  load-color description stays accurate under Catppuccin, and mention the full
  Catppuccin background.
- Note that colors assume a truecolor terminal and downsample on 16/256-color ones.

## Success criteria

- `cargo build`, `cargo test`, `cargo fmt --check`, `cargo clippy` all pass.
- `PITWALL_THEME=latte` / `macchiato` / `frappe` / `mocha` each render with the
  matching Catppuccin colors and a filled background; unset defaults to Mocha;
  a garbage value falls back to Mocha without error.
- No new dependencies; diff limited to `theme.rs` (new), `config.rs`, `app.rs`,
  `ui.rs`, `main.rs` (module decl), and `README.md`.
