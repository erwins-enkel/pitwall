use crate::model::{elapsed_secs, slice_total_bytes, Load, RunnerRow};
use crate::theme::Palette;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Cell, Gauge, Paragraph, Row, Table};
use ratatui::Frame;
use std::time::SystemTime;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

const COL_SPACING: u16 = 1;
const JOB_IDX: usize = 5;
const BRANCH_IDX: usize = 6;

/// Column constraints for the runner table. The `job` and `branch` columns are
/// `Min` so they absorb leftover terminal width; the rest are fixed. This single
/// array feeds both the rendered `Table` and `column_layout` so the width used to
/// truncate cells can never diverge from the width ratatui actually allocates.
fn column_widths() -> [Constraint; 8] {
    [
        Constraint::Length(14),
        Constraint::Length(6),
        Constraint::Length(SPARK_WIDTH as u16),
        Constraint::Length(16),
        Constraint::Length(SPARK_WIDTH as u16),
        Constraint::Min(12),
        Constraint::Min(8),
        Constraint::Length(10),
    ]
}

/// The per-column rects ratatui assigns for `area`, computed with the same
/// `Layout::horizontal(widths).flex(Flex::Start).spacing(..)` call `Table` uses
/// internally (verified against ratatui-widgets `table.rs`). Reading widths back
/// from here means we truncate to exactly what gets rendered.
fn column_layout(area: Rect) -> [Rect; 8] {
    Layout::horizontal(column_widths())
        .flex(Flex::Start)
        .spacing(COL_SPACING)
        .areas(area)
}

/// Truncate `s` to at most `max` display columns, appending `…` when it overflows
/// so the flexing `job`/`branch` columns degrade gracefully instead of ratatui
/// hard-clipping mid-word. Measured by display width so wide glyphs (CJK/emoji)
/// stay aligned; the ellipsis takes one column, so the result is at most `max` wide.
fn truncate_ellipsis(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let budget = max - 1; // leave one column for the ellipsis
    let mut out = String::new();
    let mut width = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if width + cw > budget {
            break;
        }
        out.push(c);
        width += cw;
    }
    out.push('\u{2026}');
    out
}

fn table_row(
    row: &RunnerRow,
    now: SystemTime,
    p: &Palette,
    job_w: usize,
    branch_w: usize,
) -> Row<'static> {
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
        Cell::from(mem),
        Cell::from(mem_spark),
        Cell::from(truncate_ellipsis(&job, job_w)),
        Cell::from(truncate_ellipsis(&branch, branch_w)),
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
    // Read their allocated widths back from the same layout the Table uses, then
    // truncate those cells to fit (ratatui hard-clips without an ellipsis).
    let cols = column_layout(area);
    let job_w = cols[JOB_IDX].width as usize;
    let branch_w = cols[BRANCH_IDX].width as usize;
    let rows: Vec<Row> = view
        .rows
        .iter()
        .map(|r| table_row(r, view.now, p, job_w, branch_w))
        .collect();
    let table = Table::new(rows, column_widths())
        .header(header)
        .column_spacing(COL_SPACING)
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
    let label = format!("{:.1} / {} GiB", total as f64 / GIB, cap_gib);
    let p = view.palette;
    let gauge = Gauge::default()
        .ratio(ratio)
        .label(label)
        .style(Style::new().bg(p.base))
        .gauge_style(Style::new().fg(p.gauge).bg(p.base));
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
    use crate::model::{Load, RunnerRow};
    use crate::theme::Flavor;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::SystemTime;

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
    fn column_layout_matches_ratatui_solver() {
        // Widths/positions read back from ratatui's own solver (locked from the
        // real layout). job gets the odd leftover column, branch one less.
        let c = column_layout(Rect::new(0, 0, 120, 1));
        assert_eq!((c[JOB_IDX].x, c[JOB_IDX].width), (81, 14));
        assert_eq!((c[BRANCH_IDX].x, c[BRANCH_IDX].width), (96, 13));

        let c = column_layout(Rect::new(0, 0, 200, 1));
        assert_eq!(c[JOB_IDX].width, 54);
        assert_eq!(c[BRANCH_IDX].width, 53);

        // Narrow: flex columns bottom out at their Min minimums, never underflow.
        let c = column_layout(Rect::new(0, 0, 64, 1));
        assert_eq!(c[JOB_IDX].width, 12);
        assert_eq!(c[BRANCH_IDX].width, 8);
    }

    #[test]
    fn truncate_ellipsis_fits_untouched() {
        assert_eq!(truncate_ellipsis("abc", 5), "abc");
        assert_eq!(truncate_ellipsis("abcde", 5), "abcde"); // exact fit, no ellipsis
        assert_eq!(truncate_ellipsis("abc", 0), "");
    }

    #[test]
    fn truncate_ellipsis_overflow_is_exactly_max_wide() {
        let r = truncate_ellipsis("Security \u{203a} Dependency review", 12);
        assert_eq!(UnicodeWidthStr::width(r.as_str()), 12);
        assert!(r.ends_with('\u{2026}'));
    }

    #[test]
    fn truncate_ellipsis_wide_glyphs_stay_within_max() {
        // CJK glyphs are 2 columns each; result must not exceed max.
        let r = truncate_ellipsis("\u{65e5}\u{672c}\u{8a9e} test", 6);
        assert!(UnicodeWidthStr::width(r.as_str()) <= 6);
        assert!(r.ends_with('\u{2026}'));
    }

    #[test]
    fn truncate_ellipsis_control_char_no_panic() {
        // Control chars report width None; unwrap_or(0) keeps the loop sound (no
        // panic) and bounded under the same per-char measure the function uses.
        let r = truncate_ellipsis("a\u{7}bcdefghij", 5);
        let width: usize = r
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum();
        assert!(width <= 5);
        assert!(r.ends_with('\u{2026}'));
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
                cpu_hist: vec![],
                mem_hist: vec![],
            },
        ];
        // 140 wide: the two 20-wide spark columns plus the flexible job/branch
        // columns need the extra room to keep every column visible for the asserts.
        let palette = Palette::for_flavor(Flavor::Mocha);
        let mut term = Terminal::new(TestBackend::new(140, 12)).unwrap();
        term.draw(|f| {
            render(
                f,
                &View {
                    rows: &rows,
                    slice_cap_bytes: 24 * 1024 * 1024 * 1024,
                    now: SystemTime::now(),
                    status: None,
                    palette: &palette,
                    prefix: "ci-runner-",
                    matched_seen: rows.len(),
                    unmatched_seen: 0,
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
        assert!(content.contains("ci-runner-1"));
        assert!(content.contains("idle"));
        assert!(content.contains("branch"));
        assert!(content.contains("main"));
        assert!(content.contains("elapsed"));
    }

    fn busy_row(
        name: &str,
        workflow: &str,
        job: &str,
        branch: &str,
        elapsed_secs: u64,
    ) -> RunnerRow {
        RunnerRow {
            name: name.into(),
            cpu_pct: 12.0,
            mem_bytes: 47 * 1024 * 1024,
            mem_limit: 8 * 1024 * 1024 * 1024,
            job: Some(crate::model::JobInfo {
                workflow: workflow.into(),
                job: job.into(),
                branch: branch.into(),
                started_at: SystemTime::now() - std::time::Duration::from_secs(elapsed_secs),
            }),
            load: Load::Busy,
            cpu_hist: vec![],
            mem_hist: vec![],
        }
    }

    fn render_to_string(width: u16, rows: &[RunnerRow]) -> String {
        let palette = Palette::for_flavor(Flavor::Mocha);
        let mut term = Terminal::new(TestBackend::new(width, 6)).unwrap();
        term.draw(|f| {
            render(
                f,
                &View {
                    rows,
                    slice_cap_bytes: 24 * 1024 * 1024 * 1024,
                    now: SystemTime::now(),
                    status: None,
                    palette: &palette,
                    prefix: "ci-runner-",
                    matched_seen: rows.len(),
                    unmatched_seen: 0,
                },
            );
        })
        .unwrap();
        term.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn wide_terminal_shows_full_job_branch_and_fixed_columns() {
        let rows = vec![busy_row(
            "ci-runner-1",
            "Security",
            "Dependency review",
            "feature/long-branch-name",
            5025, // 1:23:45
        )];
        // 160 wide leaves job=34 / branch=33 cols — ample for the full strings.
        let content = render_to_string(160, &rows);
        // Flexing columns fully visible (no ellipsis).
        assert!(content.contains("Security \u{203a} Dependency review"));
        assert!(content.contains("feature/long-branch-name"));
        assert!(!content.contains('\u{2026}'));
        // Fixed columns un-clipped: if the job read-back diverged from the Table's
        // real layout, the last (elapsed) column would be pushed off / clipped.
        assert!(content.contains("ci-runner-1"));
        assert!(content.contains("47.0MiB/8.0GiB"));
        assert!(content.contains("1:23:45"));
    }

    #[test]
    fn narrow_terminal_truncates_job_and_branch_with_trailing_ellipsis() {
        let rows = vec![busy_row(
            "ci-runner-1",
            "Security",
            "Dependency review",
            "feature/long-branch-name",
            30,
        )];
        let width = 64;
        let palette = Palette::for_flavor(Flavor::Mocha);
        let mut term = Terminal::new(TestBackend::new(width, 6)).unwrap();
        term.draw(|f| {
            render(
                f,
                &View {
                    rows: &rows,
                    slice_cap_bytes: 24 * 1024 * 1024 * 1024,
                    now: SystemTime::now(),
                    status: None,
                    palette: &palette,
                    prefix: "ci-runner-",
                    matched_seen: rows.len(),
                    unmatched_seen: 0,
                },
            );
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        // No banner: title y=0, header y=1, first data row y=2.
        let data_y = 2;
        let cols = column_layout(Rect::new(0, 0, width, 1));
        // job (12 wide) and branch (8 wide) are both narrower than their content,
        // so the ellipsis must be the trailing glyph at each column's last cell.
        let job_last_x = cols[JOB_IDX].x + cols[JOB_IDX].width - 1;
        let branch_last_x = cols[BRANCH_IDX].x + cols[BRANCH_IDX].width - 1;
        assert_eq!(buf[(job_last_x, data_y)].symbol(), "\u{2026}");
        assert_eq!(buf[(branch_last_x, data_y)].symbol(), "\u{2026}");
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
        assert!(content.contains("docker: unreachable"));
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
