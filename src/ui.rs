use crate::model::{
    elapsed_secs, mem_level, slice_total_bytes, HostedJob, HostedStatus, Load, MemLevel, RunnerRow,
};
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
    pub warn_ratio: f64,
    pub crit_ratio: f64,
    /// Hosted (GitHub-hosted) jobs — running + queued — shown in their own
    /// section below the runner table. Empty ⇒ section hidden.
    pub hosted: &'a [HostedJob],
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

/// Max hosted rows rendered before collapsing the rest into a `+N more` line.
const HOSTED_CAP: usize = 6;

/// Vertical cells the hosted section needs for `n` jobs: 0 when empty, else a
/// header row + up to `HOSTED_CAP` job rows + one overflow line when truncated.
fn hosted_height(n: usize) -> u16 {
    if n == 0 {
        return 0;
    }
    let shown = n.min(HOSTED_CAP) as u16;
    let overflow = if n > HOSTED_CAP { 1 } else { 0 };
    1 + shown + overflow
}

/// Compact wait/elapsed for queued jobs: `8s`, `2m`, `1h2m`.
fn fmt_wait(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
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
const RUNNER_IDX: usize = 0;
const JOB_IDX: usize = 5;
const BRANCH_IDX: usize = 6;
/// Floor for the runner column so the `runner` header never clips.
const RUNNER_HEADER_W: usize = 6; // "runner"

/// Width for the runner column: the widest runner name in `rows`, floored at the `runner`
/// header width. Content-derived so the distinguishing `-N` suffix stays visible for any
/// `PITWALL_PREFIX` — a fixed width would re-clip a longer prefix. The `Length` this feeds
/// is shrunk by ratatui's solver under space pressure (no manual bound here); the rendered
/// width is read back from `column_layout` to drive front-truncation.
fn runner_col_width(rows: &[RunnerRow]) -> u16 {
    rows.iter()
        .map(|r| UnicodeWidthStr::width(r.name.as_str()))
        .fold(RUNNER_HEADER_W, usize::max) as u16
}

/// Column constraints for the runner table. `runner` is a `Length` sized to the widest
/// name (`runner_w`, from `runner_col_width`); `job` and `branch` are `Min` so they absorb
/// leftover terminal width; the rest are fixed. This single array feeds both the rendered
/// `Table` and `column_layout` so the width used to truncate cells can never diverge from
/// the width ratatui actually allocates.
fn column_widths(runner_w: u16) -> [Constraint; 8] {
    [
        Constraint::Length(runner_w),
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
fn column_layout(area: Rect, runner_w: u16) -> [Rect; 8] {
    Layout::horizontal(column_widths(runner_w))
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

/// Like `truncate_ellipsis` but keeps the **tail**, prepending `…`, so the runner cell's
/// distinguishing numeric suffix (`…runner-1`) survives when the column is squeezed —
/// the number is exactly what tail-truncation would drop. At most `max` columns wide.
fn truncate_ellipsis_front(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    // Walk from the end, keeping as many trailing chars as fit; leave one column for `…`.
    let budget = max - 1;
    let mut kept: Vec<char> = Vec::new();
    let mut width = 0usize;
    for c in s.chars().rev() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if width + cw > budget {
            break;
        }
        kept.push(c);
        width += cw;
    }
    kept.reverse();
    let mut out = String::with_capacity(kept.len() + 1);
    out.push('\u{2026}');
    out.extend(kept);
    out
}

fn table_row(
    row: &RunnerRow,
    now: SystemTime,
    p: &Palette,
    runner_w: usize,
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
    // Uncapped runners (native cgroups, limit 0) show usage alone, not `X/0.0MiB`.
    let mem = if row.mem_limit == 0 {
        fmt_mem(row.mem_bytes)
    } else {
        format!("{}/{}", fmt_mem(row.mem_bytes), fmt_mem(row.mem_limit))
    };
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
        // Busy without detail (org runners) reads "busy"; otherwise idle.
        None if matches!(row.load, Load::Busy) => {
            ("busy".to_string(), "-".to_string(), "-".to_string())
        }
        None => (
            "\u{2014} idle".to_string(),
            "-".to_string(),
            "-".to_string(),
        ),
    };
    Row::new(vec![
        // Front-truncate so the distinguishing `-N` suffix survives a squeezed column.
        Cell::from(truncate_ellipsis_front(&row.name, runner_w)),
        Cell::from(cpu),
        Cell::from(cpu_spark),
        mem_cell,
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
    let runner_w = runner_col_width(view.rows);
    let cols = column_layout(area, runner_w);
    // Read the *rendered* widths back so truncation matches exactly what ratatui drew
    // (the solver shrinks the runner `Length` on narrow terminals).
    let runner_cell_w = cols[RUNNER_IDX].width as usize;
    let job_w = cols[JOB_IDX].width as usize;
    let branch_w = cols[BRANCH_IDX].width as usize;
    let rows: Vec<Row> = view
        .rows
        .iter()
        .map(|r| table_row(r, view.now, p, runner_cell_w, job_w, branch_w))
        .collect();
    let table = Table::new(rows, column_widths(runner_w))
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

const HOSTED_LABEL_W: u16 = 14;
const HOSTED_ELAPSED_W: u16 = 12;

/// Column rects for the hosted table: `workflow › job` (flex), `label`,
/// `branch`, `elapsed` — mirrors `column_layout`'s read-back approach so the
/// flexing cells are truncated to exactly what ratatui allocates.
fn hosted_col_layout(area: Rect) -> [Rect; 4] {
    Layout::horizontal([
        Constraint::Min(12),
        Constraint::Length(HOSTED_LABEL_W),
        Constraint::Min(8),
        Constraint::Length(HOSTED_ELAPSED_W),
    ])
    .flex(Flex::Start)
    .spacing(COL_SPACING)
    .areas(area)
}

fn hosted_row(
    j: &HostedJob,
    now: SystemTime,
    p: &Palette,
    job_w: usize,
    branch_w: usize,
) -> Row<'static> {
    let (glyph, color) = match j.status {
        HostedStatus::InProgress => ('\u{25cf}', p.busy), // ●
        HostedStatus::Queued => ('\u{25cb}', p.warn),     // ○
    };
    let wj = format!("{} {} \u{203a} {}", glyph, j.workflow, j.job);
    let branch = if j.branch.is_empty() {
        "-".to_string()
    } else {
        j.branch.clone()
    };
    let elapsed = match j.status {
        HostedStatus::InProgress => fmt_elapsed(elapsed_secs(j.since, now)),
        HostedStatus::Queued => format!("queued {}", fmt_wait(elapsed_secs(j.since, now))),
    };
    Row::new(vec![
        Cell::from(truncate_ellipsis(&wj, job_w)),
        Cell::from(truncate_ellipsis(&j.label, HOSTED_LABEL_W as usize)),
        Cell::from(truncate_ellipsis(&branch, branch_w)),
        Cell::from(elapsed),
    ])
    .style(Style::new().fg(color).bg(p.base))
}

fn render_hosted(frame: &mut Frame, area: Rect, view: &View) {
    let p = view.palette;
    let header = Row::new(vec!["hosted", "label", "branch", "elapsed"])
        .style(Style::new().fg(p.text).bg(p.base).bold());
    let cols = hosted_col_layout(area);
    let job_w = cols[0].width as usize;
    let branch_w = cols[2].width as usize;

    let n = view.hosted.len();
    let shown = n.min(HOSTED_CAP);
    let mut rows: Vec<Row> = view.hosted[..shown]
        .iter()
        .map(|j| hosted_row(j, view.now, p, job_w, branch_w))
        .collect();
    if n > HOSTED_CAP {
        rows.push(
            Row::new(vec![Cell::from(format!("+{} more", n - HOSTED_CAP))])
                .style(Style::new().fg(p.idle).bg(p.base)),
        );
    }
    let table = Table::new(
        rows,
        [
            Constraint::Min(12),
            Constraint::Length(HOSTED_LABEL_W),
            Constraint::Min(8),
            Constraint::Length(HOSTED_ELAPSED_W),
        ],
    )
    .header(header)
    .column_spacing(COL_SPACING)
    .style(Style::new().fg(p.text).bg(p.base));
    frame.render_widget(table, area);
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
    let hosted_h = hosted_height(view.hosted.len());
    let mut constraints = vec![Constraint::Length(1)];
    if has_banner {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Min(1));
    if hosted_h > 0 {
        constraints.push(Constraint::Length(hosted_h));
    }
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

    if hosted_h > 0 {
        render_hosted(frame, chunks[idx], view);
        idx += 1;
    }

    let gauge_area = chunks[idx];
    render_gauge(frame, gauge_area, view);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Load, MemLevel, RunnerRow, SourceKind};
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
            kind: SourceKind::Docker,
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
                    hosted: &[],
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
    fn column_layout_matches_ratatui_solver() {
        // Widths/positions read back from ratatui's own solver (locked from the
        // real layout) with a realistic 17-col runner request. job gets the odd
        // leftover column, branch one less.
        const RW: u16 = 17; // pulse-ci-runner-N
        let c = column_layout(Rect::new(0, 0, 120, 1), RW);
        assert_eq!(c[RUNNER_IDX].width, 17); // full name fits
        assert_eq!((c[JOB_IDX].x, c[JOB_IDX].width), (84, 12));
        assert_eq!((c[BRANCH_IDX].x, c[BRANCH_IDX].width), (97, 12));

        let c = column_layout(Rect::new(0, 0, 200, 1), RW);
        assert_eq!(c[RUNNER_IDX].width, 17);
        assert_eq!(c[JOB_IDX].width, 52);
        assert_eq!(c[BRANCH_IDX].width, 52);

        // The runner `Length` is shrunk by the solver on narrow terminals — this is
        // exactly the width the renderer reads back to front-truncate. Locking these
        // guards `runner_col_width`/render against silently diverging.
        assert_eq!(
            column_layout(Rect::new(0, 0, 113, 1), RW)[RUNNER_IDX].width,
            17
        );
        assert_eq!(
            column_layout(Rect::new(0, 0, 100, 1), RW)[RUNNER_IDX].width,
            9
        );
        assert_eq!(
            column_layout(Rect::new(0, 0, 80, 1), RW)[RUNNER_IDX].width,
            6
        );

        // Narrow: flex columns bottom out at their Min minimums, never underflow.
        let c = column_layout(Rect::new(0, 0, 64, 1), RW);
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
    fn truncate_ellipsis_front_fits_untouched() {
        assert_eq!(truncate_ellipsis_front("abc", 5), "abc");
        assert_eq!(truncate_ellipsis_front("abcde", 5), "abcde"); // exact fit, no ellipsis
        assert_eq!(truncate_ellipsis_front("abc", 0), "");
    }

    #[test]
    fn truncate_ellipsis_front_keeps_the_tail() {
        // The number is the distinguishing suffix — it must survive.
        assert_eq!(
            truncate_ellipsis_front("pulse-ci-runner-1", 9),
            "\u{2026}runner-1"
        );
        assert_eq!(
            truncate_ellipsis_front("pulse-ci-runner-1", 6),
            "\u{2026}ner-1"
        );
        let r = truncate_ellipsis_front("pulse-ci-runner-1", 9);
        assert_eq!(UnicodeWidthStr::width(r.as_str()), 9);
        assert!(r.starts_with('\u{2026}'));
        assert!(r.ends_with("-1"));
    }

    #[test]
    fn truncate_ellipsis_front_wide_glyphs_stay_within_max() {
        // Trailing CJK glyphs are 2 columns each; result must not exceed max.
        let r = truncate_ellipsis_front("test \u{65e5}\u{672c}\u{8a9e}", 6);
        assert!(UnicodeWidthStr::width(r.as_str()) <= 6);
        assert!(r.starts_with('\u{2026}'));
    }

    #[test]
    fn runner_col_width_derives_from_names() {
        // Widest name when it fits.
        let rows = vec![
            busy_row("pulse-ci-runner-1", "CI", "t", "main", 1),
            busy_row("pulse-ci-runner-10", "CI", "t", "main", 1),
        ];
        assert_eq!(runner_col_width(&rows), 18); // "pulse-ci-runner-10"
                                                 // Floors at the "runner" header width for short names / no rows.
        assert_eq!(runner_col_width(&[busy_row("r1", "CI", "t", "main", 1)]), 6);
        assert_eq!(runner_col_width(&[]), 6);
    }

    /// Reads the runner column's cells at the first data row into a trimmed string.
    fn runner_cell(rows: &[RunnerRow], width: u16) -> String {
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
                    prefix: "pulse-ci-runner-",
                    matched_seen: rows.len(),
                    unmatched_seen: 0,
                    warn_ratio: 0.85,
                    crit_ratio: 0.90,
                    hosted: &[],
                },
            );
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let cols = column_layout(Rect::new(0, 0, width, 1), runner_col_width(rows));
        let r = cols[RUNNER_IDX];
        let data_y = 2; // no banner: title y=0, header y=1, first data row y=2
        (r.x..r.x + r.width)
            .map(|x| buf[(x, data_y)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn squeezed_runner_column_keeps_the_number() {
        // The regression: at ordinary 80-110 col widths the solver shrinks runner
        // below the 17-col name, and front-truncation must preserve the trailing `-N`.
        let rows = vec![busy_row("pulse-ci-runner-1", "CI", "test", "main", 30)];
        for width in [100u16, 80] {
            let cell = runner_cell(&rows, width);
            assert!(
                cell.starts_with('\u{2026}'),
                "width {width}: runner cell should be front-truncated, got {cell:?}"
            );
            assert!(
                cell.ends_with("-1"),
                "width {width}: runner number must survive, got {cell:?}"
            );
        }
    }

    #[test]
    fn wide_runner_column_shows_full_name_with_number() {
        // The user's actual regime (~200 cols): full name, number included, no ellipsis.
        let rows = vec![busy_row("pulse-ci-runner-1", "CI", "test", "main", 30)];
        for width in [160u16, 200] {
            let cell = runner_cell(&rows, width);
            assert_eq!(cell, "pulse-ci-runner-1", "width {width}");
        }
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
                kind: SourceKind::Docker,
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
                kind: SourceKind::Docker,
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
    fn native_row_hides_zero_limit_and_shows_busy_without_detail() {
        let rows = vec![RunnerRow {
            name: "ltdovr".into(),
            cpu_pct: 12.0,
            mem_bytes: 100 * 1024 * 1024,
            mem_limit: 0, // uncapped native cgroup
            job: None,
            load: Load::Busy, // busy via org endpoint, no workflow›job
            mem_level: MemLevel::Normal,
            kind: SourceKind::Native,
            cpu_hist: vec![],
            mem_hist: vec![],
        }];
        let term = draw(&rows, 24 * 1024 * 1024 * 1024);
        let content = text(&term);
        assert!(content.contains("ltdovr"));
        assert!(content.contains("busy"));
        assert!(content.contains("100.0MiB"));
        // No `/0.0MiB` denominator for uncapped runners.
        assert!(!content.contains("/0.0MiB"));
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
            mem_level: MemLevel::Normal,
            kind: SourceKind::Docker,
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
                    warn_ratio: 0.85,
                    crit_ratio: 0.90,
                    hosted: &[],
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
                    warn_ratio: 0.85,
                    crit_ratio: 0.90,
                    hosted: &[],
                },
            );
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        // No banner: title y=0, header y=1, first data row y=2.
        let data_y = 2;
        let cols = column_layout(Rect::new(0, 0, width, 1), runner_col_width(&rows));
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
                        warn_ratio: 0.85,
                        crit_ratio: 0.90,
                        hosted: &[],
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
                    hosted: &[],
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
                    hosted: &[],
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
    fn hosted_height_is_zero_when_empty_and_caps_with_overflow() {
        assert_eq!(hosted_height(0), 0);
        assert_eq!(hosted_height(3), 1 + 3); // header + 3 rows
        assert_eq!(hosted_height(HOSTED_CAP), 1 + HOSTED_CAP as u16);
        // over cap → header + CAP rows + one "+N more" line
        assert_eq!(hosted_height(HOSTED_CAP + 5), 1 + HOSTED_CAP as u16 + 1);
    }

    #[test]
    fn fmt_wait_compact_units() {
        assert_eq!(fmt_wait(8), "8s");
        assert_eq!(fmt_wait(125), "2m");
        assert_eq!(fmt_wait(3720), "1h2m");
    }

    #[test]
    fn hosted_section_shows_running_glyph_queued_label_and_overflow_count() {
        // 8 hosted jobs > HOSTED_CAP (6): a queued row sits inside the shown
        // window (mixed with running ones) so both indicators are on-screen,
        // and the trailing 2 collapse into the "+2 more" overflow line.
        let now = SystemTime::now();
        let mut hosted = vec![HostedJob {
            workflow: "CI".into(),
            job: "Lint".into(),
            label: "ubuntu-24.04".into(),
            branch: "main".into(),
            status: HostedStatus::Queued,
            since: now - std::time::Duration::from_secs(10),
        }];
        for i in 0..7 {
            hosted.push(HostedJob {
                workflow: "CI".into(),
                job: format!("Build-{i}"),
                label: "ubuntu-latest".into(),
                branch: "main".into(),
                status: HostedStatus::InProgress,
                since: now - std::time::Duration::from_secs(30),
            });
        }
        assert_eq!(hosted.len(), 8);

        let palette = Palette::for_flavor(Flavor::Mocha);
        let mut term = Terminal::new(TestBackend::new(140, 20)).unwrap();
        term.draw(|f| {
            render(
                f,
                &View {
                    rows: &[],
                    slice_cap_bytes: 24 * 1024 * 1024 * 1024,
                    now,
                    status: None,
                    palette: &palette,
                    prefix: "ci-runner-",
                    matched_seen: 0,
                    unmatched_seen: 0,
                    warn_ratio: 0.85,
                    crit_ratio: 0.90,
                    hosted: &hosted,
                },
            );
        })
        .unwrap();
        let content = text(&term);
        assert!(content.contains('\u{25cf}'), "running glyph should render");
        assert!(
            content.contains("queued"),
            "queued elapsed cell should render"
        );
        // 8 jobs, HOSTED_CAP=6 shown → 2 collapse into the overflow line.
        assert!(
            content.contains("+2 more"),
            "overflow line should show +2 more"
        );
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
                    hosted: &[],
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
