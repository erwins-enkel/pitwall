use crate::model::{elapsed_secs, mem_level, slice_total_bytes, Load, MemLevel, RunnerRow};
use crate::theme::Palette;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Cell, Gauge, Paragraph, Row, Table};
use ratatui::Frame;
use std::time::SystemTime;

const KIB: f64 = 1024.0;
const MIB: f64 = KIB * 1024.0;
const GIB: f64 = MIB * 1024.0;

const SPARK_WIDTH: usize = 20;
const BLOCKS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];
/// CPU spark auto-scales to its window max but never below this, so idle jitter
/// (0.1–0.3%) reads as a flat baseline rather than amplified noise.
const CPU_SPARK_FLOOR: f64 = 10.0;

pub struct View<'a> {
    pub rows: &'a [RunnerRow],
    pub slice_cap_bytes: u64,
    pub now: SystemTime,
    pub status: Option<String>,
    pub palette: &'a Palette,
    /// Container name prefix (`PITWALL_PREFIX`) the resource poller filters on.
    pub prefix: &'a str,
    /// Running containers matching the prefix last poll. Non-zero with an empty
    /// table means matches exist but stats weren't ready — not a mismatch.
    pub matched_seen: usize,
    /// Running containers NOT matching the prefix last poll. Drives the
    /// prefix-mismatch hint when nothing matched.
    pub unmatched_seen: usize,
    pub warn_ratio: f64,
    pub crit_ratio: f64,
}

pub fn fmt_mem(bytes: u64) -> String {
    let bytes = bytes as f64;
    let mib = format!("{:.1}", bytes / MIB);
    if bytes >= GIB || mib == "1024.0" {
        format!("{:.1}GiB", bytes / GIB)
    } else {
        format!("{mib}MiB")
    }
}

pub fn fmt_elapsed(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Renders `values` (oldest→newest) as block-char sparkline glyphs scaled
/// against `max`, right-aligned within `width` (left-padded with spaces). A
/// non-positive `max` renders all values at the lowest level.
fn spark(values: &[f64], max: f64, width: usize) -> String {
    let start = values.len().saturating_sub(width);
    let shown = &values[start..];
    let mut s = String::with_capacity(width * 3);
    for _ in 0..width.saturating_sub(shown.len()) {
        s.push(' ');
    }
    for &v in shown {
        let level = if max > 0.0 {
            ((v / max).clamp(0.0, 1.0) * (BLOCKS.len() - 1) as f64).round() as usize
        } else {
            0
        };
        s.push(BLOCKS[level]);
    }
    s
}

fn load_style(load: Load, p: &Palette) -> Style {
    let base = Style::new().bg(p.base);
    match load {
        // DIM sharpens idle rows on a dark base but washes them out on a light
        // one, so light flavors (Latte) rely on the idle color alone.
        Load::Idle if p.is_light => base.fg(p.idle),
        Load::Idle => base.fg(p.idle).add_modifier(Modifier::DIM),
        Load::Busy => base.fg(p.busy),
        Load::NearCap => base.fg(p.near_cap),
    }
}

fn table_row(row: &RunnerRow, now: SystemTime, p: &Palette) -> Row<'static> {
    let cpu = format!("{:.1}%", row.cpu_pct);
    // CPU has no per-runner cap in the model, so scale to the window max with a
    // floor. Mem is already a 0..1 fraction of the limit, so scale to 1.0.
    let cpu_max = row
        .cpu_hist
        .iter()
        .copied()
        .fold(0.0_f64, f64::max)
        .max(CPU_SPARK_FLOOR);
    let cpu_spark = spark(&row.cpu_hist, cpu_max, SPARK_WIDTH);
    let mem = format!("{}/{}", fmt_mem(row.mem_bytes), fmt_mem(row.mem_limit));
    let mem_spark = spark(&row.mem_hist, 1.0, SPARK_WIDTH);
    // Warn memory pressure is signalled on the mem cell alone (warn color),
    // overriding the row's job-state color for that one cell so a busy runner
    // stays green. Critical is handled by the whole-row NearCap style. The base
    // background is kept so the cell matches the themed row, and DIM is cleared so
    // an idle (dimmed) runner's warn cell still shows full warn color.
    let mem_cell = if row.mem_level == MemLevel::Warn {
        Cell::from(mem).style(
            Style::new()
                .fg(p.warn)
                .bg(p.base)
                .remove_modifier(Modifier::DIM),
        )
    } else {
        Cell::from(mem)
    };
    let (job, branch, elapsed) = match &row.job {
        Some(j) => {
            let branch = if j.branch.is_empty() {
                "-".to_string()
            } else {
                j.branch.clone()
            };
            (
                format!("{} \u{203a} {}", j.workflow, j.job),
                branch,
                fmt_elapsed(elapsed_secs(j.started_at, now)),
            )
        }
        None => (
            "\u{2014} idle".to_string(),
            "-".to_string(),
            "-".to_string(),
        ),
    };
    Row::new(vec![
        Cell::from(row.name.clone()),
        Cell::from(cpu),
        Cell::from(cpu_spark),
        mem_cell,
        Cell::from(mem_spark),
        Cell::from(job),
        Cell::from(branch),
        Cell::from(elapsed),
    ])
    .style(load_style(row.load, p))
}

fn render_table(frame: &mut Frame, area: Rect, view: &View) {
    let p = view.palette;
    let header = Row::new(vec![
        "runner",
        "cpu",
        "~cpu",
        "mem",
        "~mem",
        "workflow \u{203a} job",
        "branch",
        "elapsed",
    ])
    .style(Style::new().fg(p.text).bg(p.base).bold());
    // job & branch flex to absorb slack, so the layout degrades gracefully on
    // narrow terminals instead of the fixed columns dropping off the right edge.
    let rows: Vec<Row> = view
        .rows
        .iter()
        .map(|r| table_row(r, view.now, p))
        .collect();
    let widths = [
        Constraint::Length(14),
        Constraint::Length(6),
        Constraint::Length(SPARK_WIDTH as u16),
        Constraint::Length(16),
        Constraint::Length(SPARK_WIDTH as u16),
        Constraint::Min(12),
        Constraint::Min(8),
        Constraint::Length(10),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .style(Style::new().fg(p.text).bg(p.base));
    frame.render_widget(table, area);
}

fn render_empty_state(frame: &mut Frame, area: Rect, view: &View) {
    let p = view.palette;
    // Errors are already surfaced in the banner above; avoid showing them twice.
    let message = if view.matched_seen > 0 {
        // Runners matched the prefix but their stats weren't ready this poll;
        // transient, retried every 2s. NOT a prefix mismatch.
        "waiting for runner stats\u{2026}".to_string()
    } else if view.unmatched_seen > 0 {
        // Nothing matched, but the daemon has other containers running — the
        // usual cause is a prefix (or socket) mismatch, so name it.
        format!(
            "{} containers running, none match prefix '{}'",
            view.unmatched_seen, view.prefix
        )
    } else {
        "waiting for runners\u{2026}".to_string()
    };
    let chunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .split(area);
    let paragraph = Paragraph::new(message)
        .alignment(Alignment::Center)
        .style(Style::new().fg(p.text).bg(p.base));
    frame.render_widget(paragraph, chunks[1]);
}

fn render_gauge(frame: &mut Frame, area: Rect, view: &View) {
    let total = slice_total_bytes(view.rows);
    let ratio = if view.slice_cap_bytes > 0 {
        (total as f64 / view.slice_cap_bytes as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let cap_gib = (view.slice_cap_bytes as f64 / GIB).round() as u64;
    let p = view.palette;
    // Same classifier as the per-runner mem cells, mapped onto themed roles:
    // gauge (normal) / warn / near_cap. A text marker keeps the alert from being
    // color-only.
    let (fill, marker) = match mem_level(
        total,
        view.slice_cap_bytes,
        view.warn_ratio,
        view.crit_ratio,
    ) {
        MemLevel::Normal => (p.gauge, ""),
        MemLevel::Warn => (p.warn, " \u{26a0} warn"),
        MemLevel::Critical => (p.near_cap, " \u{26a0} NEAR CAP"),
    };
    let label = format!("{:.1} / {} GiB{}", total as f64 / GIB, cap_gib, marker);
    let gauge = Gauge::default()
        .ratio(ratio)
        .label(label)
        .style(Style::new().bg(p.base))
        .gauge_style(Style::new().fg(fill).bg(p.base));
    frame.render_widget(gauge, area);
}

pub fn render(frame: &mut Frame, view: &View) {
    let p = view.palette;
    // Paint the whole frame with the Catppuccin base so any cell not covered by
    // a widget (gaps, padding) carries the theme background, not the terminal's.
    frame.render_widget(
        Block::new().style(Style::new().bg(p.base).fg(p.text)),
        frame.area(),
    );

    let has_banner = view.status.is_some();
    let mut constraints = vec![Constraint::Length(1)];
    if has_banner {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    constraints.push(Constraint::Length(1));
    let chunks = Layout::vertical(constraints).split(frame.area());

    let mut idx = 0;
    let title_area = chunks[idx];
    idx += 1;
    frame.render_widget(
        Paragraph::new("pitwall").style(Style::new().fg(p.accent).bg(p.base).bold()),
        title_area,
    );

    if has_banner {
        let banner_area = chunks[idx];
        idx += 1;
        let banner = Paragraph::new(view.status.clone().unwrap_or_default())
            .style(Style::new().fg(p.error).bg(p.base).bold());
        frame.render_widget(banner, banner_area);
    }

    let body_area = chunks[idx];
    idx += 1;
    if view.rows.is_empty() {
        render_empty_state(frame, body_area, view);
    } else {
        render_table(frame, body_area, view);
    }

    let gauge_area = chunks[idx];
    render_gauge(frame, gauge_area, view);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Load, MemLevel, RunnerRow};
    use crate::theme::{Flavor, Palette};
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;
    use std::time::SystemTime;

    const CAP: u64 = 8 * 1024 * 1024 * 1024;

    fn row(mem_bytes: u64, load: Load, mem_level: MemLevel) -> RunnerRow {
        RunnerRow {
            name: "ci-runner-1".into(),
            cpu_pct: 0.5,
            mem_bytes,
            mem_limit: CAP,
            job: None,
            load,
            mem_level,
            cpu_hist: vec![],
            mem_hist: vec![],
        }
    }

    fn draw(rows: &[RunnerRow], slice_cap_bytes: u64) -> Terminal<TestBackend> {
        // 140 wide so the two 20-col sparkline columns plus the flexible
        // job/branch columns all stay visible for the asserts.
        let palette = Palette::for_flavor(Flavor::Mocha);
        let mut term = Terminal::new(TestBackend::new(140, 12)).unwrap();
        term.draw(|f| {
            render(
                f,
                &View {
                    rows,
                    slice_cap_bytes,
                    now: SystemTime::now(),
                    status: None,
                    palette: &palette,
                    prefix: "ci-runner-",
                    matched_seen: rows.len(),
                    unmatched_seen: 0,
                    warn_ratio: 0.85,
                    crit_ratio: 0.90,
                },
            );
        })
        .unwrap();
        term
    }

    fn text(term: &Terminal<TestBackend>) -> String {
        term.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn has_fg(term: &Terminal<TestBackend>, color: Color) -> bool {
        term.backend()
            .buffer()
            .content()
            .iter()
            .any(|c| c.fg == color)
    }

    /// True if any cell painted `color` still carries the DIM modifier.
    fn any_fg_is_dim(term: &Terminal<TestBackend>, color: Color) -> bool {
        term.backend()
            .buffer()
            .content()
            .iter()
            .any(|c| c.fg == color && c.modifier.contains(Modifier::DIM))
    }

    #[test]
    fn idle_dims_on_dark_but_not_on_light() {
        let dark = Palette::for_flavor(Flavor::Mocha);
        let light = Palette::for_flavor(Flavor::Latte);
        assert!(load_style(Load::Idle, &dark)
            .add_modifier
            .contains(Modifier::DIM));
        assert!(!load_style(Load::Idle, &light)
            .add_modifier
            .contains(Modifier::DIM));
    }

    #[test]
    fn spark_maps_levels_and_pads() {
        // 0 → lowest block, max → highest block.
        assert_eq!(spark(&[0.0], 100.0, 1), "\u{2581}");
        assert_eq!(spark(&[100.0], 100.0, 1), "\u{2588}");
        // Left-padded with spaces to width, newest right-aligned.
        assert_eq!(spark(&[100.0], 100.0, 3), "  \u{2588}");
        // Empty input → all spaces.
        assert_eq!(spark(&[], 1.0, 3), "   ");
        // Non-positive max → all lowest (no divide/amplify).
        assert_eq!(spark(&[5.0, 3.0], 0.0, 2), "\u{2581}\u{2581}");
        // Over-width input keeps the most recent `width` samples.
        assert_eq!(spark(&[0.0, 100.0], 100.0, 1), "\u{2588}");
    }

    #[test]
    fn formats_mem_and_elapsed() {
        assert_eq!(fmt_mem(1024 * 1024 * 1024), "1.0GiB");
        assert_eq!(fmt_mem(1024 * 1024 * 1024 - 1), "1.0GiB"); // boundary: don't print 1024.0MiB
        assert_eq!(fmt_mem(42 * 1024 * 1024), "42.0MiB");
        assert_eq!(fmt_elapsed(75), "01:15");
        assert_eq!(fmt_elapsed(3661), "1:01:01");
    }

    #[test]
    fn renders_without_panic_and_shows_runner() {
        let rows = vec![
            RunnerRow {
                name: "ci-runner-1".into(),
                cpu_pct: 0.5,
                mem_bytes: 47 * 1024 * 1024,
                mem_limit: 8 * 1024 * 1024 * 1024,
                job: None,
                load: Load::Idle,
                mem_level: MemLevel::Normal,
                cpu_hist: vec![],
                mem_hist: vec![],
            },
            RunnerRow {
                name: "ci-runner-2".into(),
                cpu_pct: 90.0,
                mem_bytes: 47 * 1024 * 1024,
                mem_limit: 8 * 1024 * 1024 * 1024,
                job: Some(crate::model::JobInfo {
                    workflow: "CI".into(),
                    job: "test".into(),
                    branch: "main".into(),
                    started_at: SystemTime::now(),
                }),
                load: Load::Busy,
                mem_level: MemLevel::Normal,
                cpu_hist: vec![],
                mem_hist: vec![],
            },
        ];
        let term = draw(&rows, 24 * 1024 * 1024 * 1024);
        let content = text(&term);
        assert!(content.contains("ci-runner-1"));
        assert!(content.contains("idle"));
        assert!(content.contains("branch"));
        assert!(content.contains("main"));
        assert!(content.contains("elapsed"));
    }

    #[test]
    fn paints_full_catppuccin_background_for_every_flavor() {
        // Renders each flavor and asserts every cell carries that flavor's base
        // background — guards the full-bg fill across all four palettes (a live
        // TTY run isn't available in tests).
        for flavor in [
            Flavor::Mocha,
            Flavor::Macchiato,
            Flavor::Frappe,
            Flavor::Latte,
        ] {
            let palette = Palette::for_flavor(flavor);
            let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
            term.draw(|f| {
                render(
                    f,
                    &View {
                        rows: &[],
                        slice_cap_bytes: 24 * 1024 * 1024 * 1024,
                        now: SystemTime::now(),
                        status: None,
                        palette: &palette,
                        prefix: "ci-runner-",
                        matched_seen: 0,
                        unmatched_seen: 0,
                        warn_ratio: 0.85,
                        crit_ratio: 0.90,
                    },
                );
            })
            .unwrap();
            let buf = term.backend().buffer();
            assert!(
                buf.content().iter().all(|c| c.bg == palette.base),
                "all cells should have the {flavor:?} base background"
            );
        }
    }

    #[test]
    fn empty_rows_with_status_shows_banner_not_blank() {
        let palette = Palette::for_flavor(Flavor::Mocha);
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| {
            render(
                f,
                &View {
                    rows: &[],
                    slice_cap_bytes: 24 * 1024 * 1024 * 1024,
                    now: SystemTime::now(),
                    status: Some("docker: unreachable".into()),
                    palette: &palette,
                    prefix: "ci-runner-",
                    matched_seen: 0,
                    unmatched_seen: 0,
                    warn_ratio: 0.85,
                    crit_ratio: 0.90,
                },
            );
        })
        .unwrap();
        assert!(text(&term).contains("docker: unreachable"));
    }

    #[test]
    fn warn_band_busy_row_stays_green_but_mem_cell_is_warn_color() {
        // Two channels: the row is busy (green) while the mem cell alone is the
        // warn color — the warn signal does not swallow the healthy-busy color.
        let m = Palette::for_flavor(Flavor::Mocha);
        let rows = vec![row((CAP as f64 * 0.87) as u64, Load::Busy, MemLevel::Warn)];
        let term = draw(&rows, 24 * 1024 * 1024 * 1024);
        assert!(has_fg(&term, m.busy), "busy row should use the busy color");
        assert!(
            has_fg(&term, m.warn),
            "warn mem cell should use the warn color"
        );
    }

    #[test]
    fn warn_mem_cell_is_not_dimmed_on_an_idle_row() {
        // An idle runner (no job) can still sit in the warn band. Idle rows carry
        // the DIM modifier; the warn mem cell must clear it so it shows full warn
        // color rather than a dimmed yellow.
        let m = Palette::for_flavor(Flavor::Mocha);
        let rows = vec![row((CAP as f64 * 0.87) as u64, Load::Idle, MemLevel::Warn)];
        let term = draw(&rows, 24 * 1024 * 1024 * 1024);
        assert!(
            has_fg(&term, m.warn),
            "warn mem cell should use the warn color"
        );
        assert!(
            !any_fg_is_dim(&term, m.warn),
            "warn mem cell must not carry the row's DIM modifier"
        );
    }

    #[test]
    fn critical_row_is_near_cap_color() {
        let m = Palette::for_flavor(Flavor::Mocha);
        let rows = vec![row(
            (CAP as f64 * 0.95) as u64,
            Load::NearCap,
            MemLevel::Critical,
        )];
        let term = draw(&rows, 24 * 1024 * 1024 * 1024);
        assert!(
            has_fg(&term, m.near_cap),
            "critical row should use the near_cap color"
        );
    }

    #[test]
    fn gauge_is_normal_color_and_unmarked_when_normal() {
        // Slice total well below warn: 1 GiB of a 24 GiB cap.
        let m = Palette::for_flavor(Flavor::Mocha);
        let rows = vec![row(1024 * 1024 * 1024, Load::Busy, MemLevel::Normal)];
        let term = draw(&rows, 24 * 1024 * 1024 * 1024);
        assert!(has_fg(&term, m.gauge), "gauge should use the normal color");
        let content = text(&term);
        assert!(!content.contains("warn"));
        assert!(!content.contains("NEAR CAP"));
    }

    #[test]
    fn gauge_warn_color_with_marker_in_warn_band() {
        // Slice total 87% of a 4 GiB cap → warn band.
        let m = Palette::for_flavor(Flavor::Mocha);
        let cap = 4 * 1024 * 1024 * 1024;
        let rows = vec![row((cap as f64 * 0.87) as u64, Load::Busy, MemLevel::Warn)];
        let term = draw(&rows, cap);
        assert!(has_fg(&term, m.warn), "gauge should use the warn color");
        assert!(text(&term).contains("\u{26a0} warn"));
    }

    #[test]
    fn gauge_red_with_marker_when_critical() {
        // Slice total 95% of a 4 GiB cap → critical.
        let m = Palette::for_flavor(Flavor::Mocha);
        let cap = 4 * 1024 * 1024 * 1024;
        let rows = vec![row(
            (cap as f64 * 0.95) as u64,
            Load::NearCap,
            MemLevel::Critical,
        )];
        let term = draw(&rows, cap);
        assert!(
            has_fg(&term, m.near_cap),
            "gauge should use the near_cap color"
        );
        assert!(text(&term).contains("\u{26a0} NEAR CAP"));
    }

    #[test]
    fn empty_rows_with_unmatched_containers_names_the_prefix() {
        let palette = Palette::for_flavor(Flavor::Mocha);
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| {
            render(
                f,
                &View {
                    rows: &[],
                    slice_cap_bytes: 24 * 1024 * 1024 * 1024,
                    now: SystemTime::now(),
                    status: None,
                    palette: &palette,
                    prefix: "ci-runner-",
                    matched_seen: 0,
                    unmatched_seen: 6,
                    warn_ratio: 0.85,
                    crit_ratio: 0.90,
                },
            );
        })
        .unwrap();
        let content = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(content.contains("6 containers running"));
        assert!(content.contains("ci-runner-"));
    }

    #[test]
    fn empty_rows_with_matched_but_no_stats_shows_stats_pending() {
        // Runners matched the prefix but stats weren't ready → must NOT claim a
        // prefix mismatch (the critic's false-"none match" case).
        let palette = Palette::for_flavor(Flavor::Mocha);
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| {
            render(
                f,
                &View {
                    rows: &[],
                    slice_cap_bytes: 24 * 1024 * 1024 * 1024,
                    now: SystemTime::now(),
                    status: None,
                    palette: &palette,
                    prefix: "ci-runner-",
                    matched_seen: 3,
                    unmatched_seen: 2,
                    warn_ratio: 0.85,
                    crit_ratio: 0.90,
                },
            );
        })
        .unwrap();
        let content = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(content.contains("waiting for runner stats"));
        assert!(!content.contains("none match"));
    }
}
