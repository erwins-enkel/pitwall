use crate::model::{elapsed_secs, slice_total_bytes, Load, RunnerRow};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Cell, Gauge, Paragraph, Row, Table};
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

fn load_style(load: Load) -> Style {
    match load {
        Load::Idle => Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        Load::Busy => Style::new().fg(Color::Green),
        Load::NearCap => Style::new().fg(Color::Red),
    }
}

fn table_row(row: &RunnerRow, now: SystemTime) -> Row<'static> {
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
        Cell::from(job),
        Cell::from(branch),
        Cell::from(elapsed),
    ])
    .style(load_style(row.load))
}

fn render_table(frame: &mut Frame, area: Rect, view: &View) {
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
    .style(Style::new().bold());
    let rows: Vec<Row> = view.rows.iter().map(|r| table_row(r, view.now)).collect();
    // job & branch flex to absorb slack, so the layout degrades gracefully on
    // narrow terminals instead of the fixed columns dropping off the right edge.
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
    let table = Table::new(rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, area);
}

fn render_empty_state(frame: &mut Frame, area: Rect) {
    // Errors are already surfaced in the banner above; avoid showing them twice.
    let message = "waiting for runners\u{2026}".to_string();
    let chunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .split(area);
    let paragraph = Paragraph::new(message).alignment(Alignment::Center);
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
    let gauge = Gauge::default()
        .ratio(ratio)
        .label(label)
        .gauge_style(Style::new().fg(Color::Cyan));
    frame.render_widget(gauge, area);
}

pub fn render(frame: &mut Frame, view: &View) {
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
    frame.render_widget(Paragraph::new("pitwall"), title_area);

    if has_banner {
        let banner_area = chunks[idx];
        idx += 1;
        let banner = Paragraph::new(view.status.clone().unwrap_or_default())
            .style(Style::new().fg(Color::Red).bold());
        frame.render_widget(banner, banner_area);
    }

    let body_area = chunks[idx];
    idx += 1;
    if view.rows.is_empty() {
        render_empty_state(frame, body_area);
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
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::SystemTime;

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
        let mut term = Terminal::new(TestBackend::new(140, 12)).unwrap();
        term.draw(|f| {
            render(
                f,
                &View {
                    rows: &rows,
                    slice_cap_bytes: 24 * 1024 * 1024 * 1024,
                    now: SystemTime::now(),
                    status: None,
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

    #[test]
    fn empty_rows_with_status_shows_banner_not_blank() {
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| {
            render(
                f,
                &View {
                    rows: &[],
                    slice_cap_bytes: 24 * 1024 * 1024 * 1024,
                    now: SystemTime::now(),
                    status: Some("docker: unreachable".into()),
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
}
