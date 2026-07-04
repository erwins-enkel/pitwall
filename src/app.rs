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
    let mut res_alive = true;
    let mut jobs_alive = true;

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
            res = rx_res.recv(), if res_alive => match res {
                Some(update) => apply_resource_update(&mut state, update),
                None => res_alive = false,
            },
            jobs = rx_jobs.recv(), if jobs_alive => match jobs {
                Some(update) => apply_jobs_update(&mut state, update),
                None => jobs_alive = false,
            },
            _ = ticker.tick() => {}
        }
        draw(&mut terminal, &state, slice_cap_bytes)?;
    }
}

/// Applies a resource poll result: the error banner always reflects the
/// latest poll, but the data table only replaces on success (`error: None`)
/// so a transient failure never wipes last-known-good rows.
fn apply_resource_update(state: &mut AppState, update: ResourceUpdate) {
    state.resource_err = update.error;
    if state.resource_err.is_none() {
        state.resources = update.resources;
    }
}

/// Applies a jobs poll result with the same error-preserves-data semantics
/// as `apply_resource_update`.
fn apply_jobs_update(state: &mut AppState, update: JobsUpdate) {
    state.jobs_err = update.error;
    if state.jobs_err.is_none() {
        state.jobs = update.jobs;
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
    let rows = join(state.resources.clone(), &state.jobs);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(name: &str) -> RunnerResource {
        RunnerResource {
            name: name.into(),
            cpu_pct: 1.0,
            mem_bytes: 100,
            mem_limit: 8 * 1024 * 1024 * 1024,
        }
    }

    fn job() -> JobInfo {
        JobInfo {
            workflow: "ci".into(),
            job: "test".into(),
            started_at: SystemTime::now(),
        }
    }

    #[test]
    fn resource_error_preserves_last_known() {
        let mut state = AppState {
            resources: vec![resource("ci-runner-1"), resource("ci-runner-2")],
            ..Default::default()
        };

        apply_resource_update(
            &mut state,
            ResourceUpdate {
                resources: vec![],
                error: Some("docker: x".to_string()),
            },
        );

        assert_eq!(state.resources.len(), 2);
        assert_eq!(state.resource_err, Some("docker: x".to_string()));
    }

    #[test]
    fn resource_success_replaces() {
        let mut state = AppState {
            resources: vec![resource("ci-runner-1"), resource("ci-runner-2")],
            resource_err: Some("stale error".to_string()),
            ..Default::default()
        };

        apply_resource_update(
            &mut state,
            ResourceUpdate {
                resources: vec![resource("ci-runner-1")],
                error: None,
            },
        );

        assert_eq!(state.resources.len(), 1);
        assert!(state.resource_err.is_none());
    }

    #[test]
    fn jobs_error_preserves_last_known() {
        let mut jobs = HashMap::new();
        jobs.insert(1u32, job());
        let mut state = AppState {
            jobs,
            ..Default::default()
        };

        apply_jobs_update(
            &mut state,
            JobsUpdate {
                jobs: HashMap::new(),
                error: Some("gh: x".to_string()),
            },
        );

        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs_err, Some("gh: x".to_string()));
    }

    #[test]
    fn jobs_success_replaces() {
        let mut stale = HashMap::new();
        stale.insert(1u32, job());
        let mut state = AppState {
            jobs: stale,
            jobs_err: Some("stale error".to_string()),
            ..Default::default()
        };

        let mut fresh = HashMap::new();
        fresh.insert(2u32, job());

        apply_jobs_update(
            &mut state,
            JobsUpdate {
                jobs: fresh,
                error: None,
            },
        );

        assert_eq!(state.jobs.len(), 1);
        assert!(state.jobs.contains_key(&2));
        assert!(state.jobs_err.is_none());
    }
}
