use crate::config::Config;
use crate::jobs::{self, JobsUpdate};
use crate::model::{join, JobInfo, RunnerResource};
use crate::resource::{self, ResourceUpdate};
use crate::ui::{self, View};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use tokio::time::interval;

#[derive(Default)]
struct AppState {
    resources: Vec<RunnerResource>,
    jobs: HashMap<u32, JobInfo>,
    resource_err: Option<String>,
    jobs_err: Option<String>,
}

/// Runs the pitwall event loop: spawns the resource/jobs pollers, then drives a
/// `tokio::select!` over terminal input, their updates, and a 1s redraw tick.
/// Degradation: a source error never clears last-known-good data — only the
/// two `*_err` fields change, and the newest error (docker takes precedence
/// over gh) becomes the status banner passed to `ui::render`.
pub async fn run(mut terminal: ratatui::DefaultTerminal) -> anyhow::Result<()> {
    let cfg = Config::from_env();
    let slice_cap_bytes = cfg.slice_cap_bytes;

    let (tx_res, mut rx_res) = mpsc::channel::<ResourceUpdate>(8);
    let (tx_jobs, mut rx_jobs) = mpsc::channel::<JobsUpdate>(8);

    tokio::spawn(resource::run(cfg.clone(), tx_res));
    tokio::spawn(jobs::run(cfg.clone(), tx_jobs));

    let mut state = AppState::default();
    let mut events = EventStream::new();
    let mut ticker = interval(Duration::from_secs(1));

    draw(&mut terminal, &state, slice_cap_bytes)?;

    loop {
        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if is_quit(&key) => return Ok(()),
                    Some(Ok(_)) => {}
                    Some(Err(_)) => {}
                    None => return Ok(()), // input stream closed
                }
            }
            Some(update) = rx_res.recv() => {
                state.resources = update.resources;
                state.resource_err = update.error;
            }
            Some(update) = rx_jobs.recv() => {
                state.jobs = update.jobs;
                state.jobs_err = update.error;
            }
            _ = ticker.tick() => {}
        }
        draw(&mut terminal, &state, slice_cap_bytes)?;
    }
}

fn is_quit(key: &KeyEvent) -> bool {
    if key.kind != KeyEventKind::Press {
        return false;
    }
    matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
}

fn draw(
    terminal: &mut ratatui::DefaultTerminal,
    state: &AppState,
    slice_cap_bytes: u64,
) -> anyhow::Result<()> {
    // Docker errors take precedence over gh errors when both are present.
    let status = state
        .resource_err
        .clone()
        .or_else(|| state.jobs_err.clone());
    let rows = join(state.resources.clone(), &state.jobs, SystemTime::now());
    terminal.draw(|f| {
        ui::render(
            f,
            &View {
                rows: &rows,
                slice_cap_bytes,
                now: SystemTime::now(),
                status,
            },
        );
    })?;
    Ok(())
}
