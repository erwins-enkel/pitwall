//! Generates a faithful, deterministic screenshot of pitwall's output with dummy
//! data, so the repo can show what the tool looks like without a live Docker
//! socket + `gh`. It builds synthetic runner rows spanning every visual state
//! (idle / busy / warn / near-cap, docker + native), renders them through the
//! *real* `ui::render` into a ratatui `TestBackend` buffer, then serializes that
//! buffer's cells to an SVG (using the real Catppuccin palette). `make
//! screenshot` pipes the SVG through `rsvg-convert` to produce `docs/pitwall.png`.
//!
//! Run: `cargo run --release --example screenshot > out.svg`
//!
//! The two required fonts are read at generation time and embedded as
//! `@font-face` data-URIs, so the PNG never bakes tofu boxes: a missing font is a
//! hard error, not a silent fallback. Override the font directory with
//! `PITWALL_SCREENSHOT_FONT_DIR` (default `/usr/share/fonts/TTF`).

use pitwall::model::{mem_level, JobInfo, Load, MemLevel, RunnerRow, SourceKind};
use pitwall::theme::{Flavor, Palette};
use pitwall::ui::{render, View};
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use std::f64::consts::PI;
use std::time::{Duration, SystemTime};

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const WARN: f64 = 0.85;
const CRIT: f64 = 0.90;
const SLICE_CAP: u64 = 24 * GIB;

// Terminal grid: 160 wide keeps the sparklines plus the full `workflow › job`
// and `branch` columns visible without ellipsis truncation (see ui.rs
// `wide_terminal_shows_full_job_branch…`). Height 9 = title + header + 6 rows +
// gauge, with no blank rows.
const COLS: u16 = 160;
const ROWS: u16 = 9;

// SVG cell metrics. JetBrains Mono's advance is 0.6em, so CW = 0.6 * FS keeps
// glyphs from overlapping; each glyph is still centered in its cell so alignment
// never drifts with font metrics.
const FS: f64 = 16.0;
const CW: f64 = 9.6;
const CH: f64 = 20.0;
const PAD: f64 = 16.0;
const BASELINE: f64 = 15.0; // from the cell's top edge

fn main() {
    let rows = demo_rows();
    let palette = Palette::for_flavor(Flavor::Mocha);
    // Fixed clock so elapsed durations are deterministic across runs.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

    let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).expect("terminal");
    term.draw(|f| {
        render(
            f,
            &View {
                rows: &rows,
                slice_cap_bytes: SLICE_CAP,
                now,
                status: None,
                palette: &palette,
                prefix: "ci-runner-",
                matched_seen: rows.len(),
                unmatched_seen: 0,
                warn_ratio: WARN,
                crit_ratio: CRIT,
            },
        );
    })
    .expect("draw");

    let svg = buffer_to_svg(term.backend().buffer(), &palette);
    print!("{svg}");
}

/// Six synthetic runners covering the full range of states, both source kinds.
/// `load`/`mem_level` are derived exactly as `model::join` does, so the demoed
/// colors and gauge banding match the chosen `mem_bytes`.
fn demo_rows() -> Vec<RunnerRow> {
    vec![
        // Docker, busy & healthy.
        docker_row(
            "ci-runner-1",
            63.2,
            34 * GIB / 10, // 3.4 GiB
            Some(("CI", "build", "main", 8 * 60 + 12)),
            wave(20, 30.0, 88.0, 2.4, 0.0),
            wave(20, 0.30, 0.46, 1.6, 0.2),
        ),
        // Docker, idle (dimmed row, flat baseline sparklines).
        docker_row(
            "ci-runner-2",
            0.2,
            348 * MIB,
            None,
            wave(20, 0.1, 0.4, 1.0, 0.0),
            flat(20, 0.04),
        ),
        // Docker, busy in the WARN band: row stays green, mem cell turns yellow.
        docker_row(
            "ci-runner-3",
            88.5,
            (8.0 * GIB as f64 * 0.87) as u64,
            Some(("Security", "Dependency review", "renovate/deps", 3 * 60 + 5)),
            wave(20, 55.0, 96.0, 3.1, 0.4),
            ramp(20, 0.62, 0.87),
        ),
        // Docker, NEAR CAP: whole row goes red.
        docker_row(
            "ci-runner-4",
            74.1,
            (8.0 * GIB as f64 * 0.953) as u64,
            Some(("Release", "publish crate", "v1.4.0", 12 * 60 + 40)),
            wave(20, 40.0, 80.0, 2.0, 0.7),
            ramp(20, 0.80, 0.95),
        ),
        // Native (non-docker) runner, busy. No cgroup limit → mem shows usage
        // alone; mem sparkline is flat (nothing to scale against).
        native_row(
            "ltdovr",
            21.0,
            512 * MIB,
            Some(("Deploy", "migrate db", "main", 90)),
            wave(20, 8.0, 34.0, 1.7, 0.1),
            flat(20, 0.0),
        ),
        // Native runner, idle.
        native_row(
            "scoop-vanscout",
            0.4,
            96 * MIB,
            None,
            wave(20, 0.1, 0.5, 1.2, 0.3),
            flat(20, 0.0),
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn make_row(
    name: &str,
    kind: SourceKind,
    cpu_pct: f64,
    mem_bytes: u64,
    mem_limit: u64,
    job: Option<(&str, &str, &str, u64)>,
    cpu_hist: Vec<f64>,
    mem_hist: Vec<f64>,
) -> RunnerRow {
    // Fixed clock (matches `main`'s `now`) so elapsed is deterministic.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let job = job.map(|(workflow, j, branch, elapsed)| JobInfo {
        workflow: workflow.into(),
        job: j.into(),
        branch: branch.into(),
        started_at: now - Duration::from_secs(elapsed),
    });
    // Derive load / mem_level exactly as model::join.
    let level = mem_level(mem_bytes, mem_limit, WARN, CRIT);
    let load = if level == MemLevel::Critical {
        Load::NearCap
    } else if job.is_some() {
        Load::Busy
    } else {
        Load::Idle
    };
    RunnerRow {
        name: name.into(),
        cpu_pct,
        mem_bytes,
        mem_limit,
        job,
        load,
        mem_level: level,
        kind,
        cpu_hist,
        mem_hist,
    }
}

fn docker_row(
    name: &str,
    cpu_pct: f64,
    mem_bytes: u64,
    job: Option<(&str, &str, &str, u64)>,
    cpu_hist: Vec<f64>,
    mem_hist: Vec<f64>,
) -> RunnerRow {
    make_row(
        name,
        SourceKind::Docker,
        cpu_pct,
        mem_bytes,
        8 * GIB,
        job,
        cpu_hist,
        mem_hist,
    )
}

fn native_row(
    name: &str,
    cpu_pct: f64,
    mem_bytes: u64,
    job: Option<(&str, &str, &str, u64)>,
    cpu_hist: Vec<f64>,
    mem_hist: Vec<f64>,
) -> RunnerRow {
    // mem_limit 0 = uncapped native cgroup → mem renders as usage alone.
    make_row(
        name,
        SourceKind::Native,
        cpu_pct,
        mem_bytes,
        0,
        job,
        cpu_hist,
        mem_hist,
    )
}

/// Deterministic sparkline data: a raised cosine sweeping `lo`→`hi` over
/// `cycles` periods. No RNG, so the screenshot is byte-stable across runs.
fn wave(len: usize, lo: f64, hi: f64, cycles: f64, phase: f64) -> Vec<f64> {
    let n = (len.max(2) - 1) as f64;
    (0..len)
        .map(|i| {
            let t = i as f64 / n;
            let s = 0.5 - 0.5 * (2.0 * PI * (cycles * t + phase)).cos();
            lo + (hi - lo) * s
        })
        .collect()
}

/// A gentle linear ramp `lo`→`hi` with a slight wave for visual life.
fn ramp(len: usize, lo: f64, hi: f64) -> Vec<f64> {
    let n = (len.max(2) - 1) as f64;
    (0..len)
        .map(|i| {
            let t = i as f64 / n;
            let wobble = 0.02 * (2.0 * PI * 3.0 * t).sin();
            (lo + (hi - lo) * t + wobble).clamp(0.0, 1.0)
        })
        .collect()
}

fn flat(len: usize, v: f64) -> Vec<f64> {
    vec![v; len]
}

// ---- buffer → SVG ---------------------------------------------------------

fn buffer_to_svg(buf: &ratatui::buffer::Buffer, palette: &Palette) -> String {
    let base = hex(palette.base, "#1e1e2e");
    let text = hex(palette.text, "#cdd6f4");
    let w = buf.area.width;
    let h = buf.area.height;
    let img_w = PAD * 2.0 + w as f64 * CW;
    let img_h = PAD * 2.0 + h as f64 * CH;

    let mut out = String::with_capacity(64 * 1024);
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{img_w:.0}\" height=\"{img_h:.0}\" \
         viewBox=\"0 0 {img_w:.0} {img_h:.0}\">\n"
    ));
    out.push_str("<defs><style>\n");
    out.push_str(&font_face("normal"));
    out.push_str(&font_face("bold"));
    out.push_str(
        "text{font-family:'PitwallMono',monospace;white-space:pre;dominant-baseline:alphabetic;}\n",
    );
    out.push_str("</style></defs>\n");
    // Whole-image base fill (matches render's full-frame Catppuccin background).
    out.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{img_w:.0}\" height=\"{img_h:.0}\" fill=\"{base}\"/>\n"
    ));

    // Background rects: coalesce horizontal runs of the same non-base bg per row
    // to avoid hairline anti-alias seams between adjacent cells.
    for y in 0..h {
        let mut run_start: Option<u16> = None;
        let mut run_hex = String::new();
        for x in 0..=w {
            let cell_hex = if x < w {
                hex(buf[(x, y)].bg, &base)
            } else {
                String::new() // sentinel to flush the final run
            };
            let same = run_start.is_some() && cell_hex == run_hex && cell_hex != base;
            if same {
                continue;
            }
            if let Some(sx) = run_start {
                if run_hex != base {
                    let rx = PAD + sx as f64 * CW;
                    let ry = PAD + y as f64 * CH;
                    let rw = (x - sx) as f64 * CW;
                    out.push_str(&format!(
                        "<rect x=\"{rx:.1}\" y=\"{ry:.1}\" width=\"{rw:.1}\" height=\"{CH:.1}\" fill=\"{run_hex}\"/>\n"
                    ));
                }
                run_start = None;
            }
            if x < w && cell_hex != base {
                run_start = Some(x);
                run_hex = cell_hex;
            }
        }
    }

    // Glyphs, one <text> per non-blank cell, centered in its cell.
    for y in 0..h {
        for x in 0..w {
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            if sym.trim().is_empty() {
                continue;
            }
            let fill = hex(cell.fg, &text);
            let tx = PAD + x as f64 * CW + CW / 2.0;
            let ty = PAD + y as f64 * CH + BASELINE;
            let mut attrs = String::new();
            if cell.modifier.contains(Modifier::BOLD) {
                attrs.push_str(" font-weight=\"bold\"");
            }
            if cell.modifier.contains(Modifier::DIM) {
                attrs.push_str(" fill-opacity=\"0.55\"");
            }
            out.push_str(&format!(
                "<text x=\"{tx:.2}\" y=\"{ty:.2}\" font-size=\"{FS}\" text-anchor=\"middle\" fill=\"{fill}\"{attrs}>{}</text>\n",
                xml_escape(sym)
            ));
        }
    }

    out.push_str("</svg>\n");
    out
}

fn font_face(weight: &str) -> String {
    let file = if weight == "bold" {
        "JetBrainsMonoNerdFontMono-Bold.ttf"
    } else {
        "JetBrainsMonoNerdFontMono-Regular.ttf"
    };
    let dir = std::env::var("PITWALL_SCREENSHOT_FONT_DIR")
        .unwrap_or_else(|_| "/usr/share/fonts/TTF".to_string());
    let path = format!("{dir}/{file}");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!(
            "screenshot: cannot read required font {path}: {e}\n\
             Install JetBrains Mono Nerd Font, or set PITWALL_SCREENSHOT_FONT_DIR."
        );
        std::process::exit(1);
    });
    format!(
        "@font-face{{font-family:'PitwallMono';font-weight:{weight};font-style:normal;\
         src:url('data:font/ttf;base64,{}') format('truetype');}}\n",
        base64(&bytes)
    )
}

fn hex(c: Color, fallback: &str) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => fallback.to_string(),
    }
}

fn xml_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            _ => c.to_string(),
        })
        .collect()
}

/// Minimal standard-alphabet base64 with padding — avoids pulling in a crate for
/// a one-shot generator.
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
