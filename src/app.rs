use crate::config::Config;
use crate::history::History;
use crate::jobs::{self, JobsUpdate};
use crate::model::{join, HostedJob, JobInfo, RunnerKey, RunnerResource, SourceKind};
use crate::resource::ResourceUpdate;
use crate::resource_native::discover;
use crate::theme::Palette;
use crate::ui::{self, View};
use crate::{resource_docker, resource_native};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use tokio::time::interval;

#[derive(Default)]
struct AppState {
    docker_resources: Vec<RunnerResource>,
    native_resources: Vec<RunnerResource>,
    docker_err: Option<String>,
    native_err: Option<String>,
    jobs: HashMap<RunnerKey, Option<JobInfo>>,
    jobs_err: Option<String>,
    hosted: Vec<HostedJob>,
    history: History,
    /// From the last successful docker poll: containers whose name matched the
    /// prefix, and those that didn't. Drives the empty-state hint (a docker-only
    /// concept — native runners have no prefix).
    matched_seen: usize,
    unmatched_seen: usize,
}

impl AppState {
    /// Every currently-known runner across both sources — the join input and the
    /// history sample set.
    fn all_resources(&self) -> Vec<RunnerResource> {
        let mut all = self.docker_resources.clone();
        all.extend(self.native_resources.clone());
        all
    }
}

/// Runs the pitwall event loop: discovers native runners once, spawns the
/// docker + native resource pollers and the jobs poller, then drives a
/// `tokio::select!` over terminal input, their updates, and a 1s redraw tick.
/// Degradation: a source error never clears last-known-good data — docker and
/// native resources keep independent last-known slices, jobs preservation lives
/// in the poller, and the newest error (docker → native → jobs) becomes the
/// status banner passed to `ui::render`.
pub async fn run(mut terminal: ratatui::DefaultTerminal, mut cfg: Config) -> anyhow::Result<()> {
    let slice_cap_bytes = cfg.slice_cap_bytes;
    let palette = Palette::for_flavor(cfg.flavor);
    let prefix = cfg.prefix.clone();
    let warn_ratio = cfg.warn_ratio;
    let crit_ratio = cfg.crit_ratio;

    // Discover native runners once; derive the jobs poll-lists from their scopes.
    let natives = discover();
    let (repos, orgs) = resource_native::derive_scopes(&cfg.configured_repos, &natives);
    cfg.repos = repos;
    cfg.orgs = orgs;
    // Gate the hosted `repo` column: only repo scopes can surface hosted jobs, so
    // the column is worth showing exactly when more than one repo is polled.
    let multi_repo = cfg.repos.len() > 1;

    let (tx_res, mut rx_res) = mpsc::channel::<ResourceUpdate>(8);
    let (tx_jobs, mut rx_jobs) = mpsc::channel::<JobsUpdate>(8);

    tokio::spawn(resource_docker::run(cfg.clone(), tx_res.clone()));
    tokio::spawn(resource_native::run(natives, tx_res));
    tokio::spawn(jobs::run(cfg.clone(), tx_jobs));

    let mut state = AppState::default();
    let mut events = EventStream::new();
    let mut ticker = interval(Duration::from_secs(1));
    let mut res_alive = true;
    let mut jobs_alive = true;

    draw(
        &mut terminal,
        &state,
        slice_cap_bytes,
        &palette,
        &prefix,
        warn_ratio,
        crit_ratio,
        multi_repo,
    )?;

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
        draw(
            &mut terminal,
            &state,
            slice_cap_bytes,
            &palette,
            &prefix,
            warn_ratio,
            crit_ratio,
            multi_repo,
        )?;
    }
}

/// Applies a resource poll result to the slice named by `update.source`, and (on
/// applied data) appends a history sample for *only the updated source's* runners
/// — so each runner gets one sample per its own 2s poll (the ~40s window holds)
/// — while pruning against the union so the other source's series survive.
///
/// The two sources handle their `error` differently, matching what an error
/// means for each. A **docker** error is a top-level list/connect failure: the
/// whole poll is invalid, so the last-known-good docker slice is preserved and
/// nothing is recorded. A **native** error only names the individual runners a
/// cgroup read failed for; the poller still sends the complete healthy set, so
/// it is applied (and recorded) regardless of the banner — one failed runner
/// never freezes the healthy rows.
fn apply_resource_update(state: &mut AppState, update: ResourceUpdate) {
    let applied = match update.source {
        SourceKind::Docker => {
            state.docker_err = update.error;
            if state.docker_err.is_none() {
                state.docker_resources = update.resources;
                // matched/unmatched are a docker-prefix concept only.
                state.matched_seen = update.matched_seen;
                state.unmatched_seen = update.unmatched_seen;
                true
            } else {
                false
            }
        }
        SourceKind::Native => {
            state.native_err = update.error;
            state.native_resources = update.resources;
            true
        }
    };
    if applied {
        let all = state.all_resources();
        let sample = match update.source {
            SourceKind::Docker => &state.docker_resources,
            SourceKind::Native => &state.native_resources,
        }
        .clone();
        state.history.record(&sample, &all);
    }
}

/// Applies a jobs poll result. Per-scope last-known-good preservation lives in
/// `jobs::run`, so here we simply replace both the data and the banner.
fn apply_jobs_update(state: &mut AppState, update: JobsUpdate) {
    state.jobs_err = update.error;
    state.jobs = update.jobs;
    state.hosted = update.hosted;
}

fn is_quit(key: &KeyEvent) -> bool {
    if key.kind != KeyEventKind::Press {
        return false;
    }
    matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
}

#[allow(clippy::too_many_arguments)]
fn draw(
    terminal: &mut ratatui::DefaultTerminal,
    state: &AppState,
    slice_cap_bytes: u64,
    palette: &Palette,
    prefix: &str,
    warn_ratio: f64,
    crit_ratio: f64,
    multi_repo: bool,
) -> anyhow::Result<()> {
    // Banner precedence: docker → native → jobs.
    let status = state
        .docker_err
        .clone()
        .or_else(|| state.native_err.clone())
        .or_else(|| state.jobs_err.clone());
    let rows = join(
        state.all_resources(),
        &state.jobs,
        &state.history,
        warn_ratio,
        crit_ratio,
    );
    terminal.draw(|f| {
        ui::render(
            f,
            &View {
                rows: &rows,
                slice_cap_bytes,
                now: SystemTime::now(),
                status,
                palette,
                prefix,
                matched_seen: state.matched_seen,
                unmatched_seen: state.unmatched_seen,
                warn_ratio,
                crit_ratio,
                hosted: &state.hosted,
                multi_repo,
            },
        );
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HostedStatus;

    fn resource(name: &str, kind: SourceKind) -> RunnerResource {
        RunnerResource {
            name: name.into(),
            cpu_pct: 1.0,
            mem_bytes: 100,
            mem_limit: 8 * 1024 * 1024 * 1024,
            key: None,
            kind,
        }
    }

    fn docker_update(resources: Vec<RunnerResource>, error: Option<String>) -> ResourceUpdate {
        let (m, u) = if error.is_none() {
            (resources.len(), 0)
        } else {
            (0, 0)
        };
        ResourceUpdate {
            source: SourceKind::Docker,
            resources,
            matched_seen: m,
            unmatched_seen: u,
            error,
        }
    }

    fn native_update(resources: Vec<RunnerResource>, error: Option<String>) -> ResourceUpdate {
        ResourceUpdate {
            source: SourceKind::Native,
            resources,
            matched_seen: 0,
            unmatched_seen: 0,
            error,
        }
    }

    fn job() -> JobInfo {
        JobInfo {
            workflow: "ci".into(),
            job: "test".into(),
            branch: "main".into(),
            started_at: SystemTime::now(),
        }
    }

    #[test]
    fn docker_error_preserves_last_known_and_leaves_native_untouched() {
        let mut state = AppState {
            docker_resources: vec![
                resource("pulse-ci-runner-1", SourceKind::Docker),
                resource("pulse-ci-runner-2", SourceKind::Docker),
            ],
            native_resources: vec![resource("ltdovr", SourceKind::Native)],
            ..Default::default()
        };

        apply_resource_update(
            &mut state,
            docker_update(vec![], Some("docker: x".to_string())),
        );

        assert_eq!(state.docker_resources.len(), 2);
        assert_eq!(state.docker_err, Some("docker: x".to_string()));
        // Native slice untouched by a docker failure.
        assert_eq!(state.native_resources.len(), 1);
    }

    #[test]
    fn resource_success_replaces_its_own_slice() {
        let mut state = AppState {
            native_resources: vec![
                resource("ltdovr", SourceKind::Native),
                resource("scoop-vanscout", SourceKind::Native),
            ],
            native_err: Some("stale error".to_string()),
            ..Default::default()
        };

        apply_resource_update(
            &mut state,
            native_update(vec![resource("ltdovr", SourceKind::Native)], None),
        );

        assert_eq!(state.native_resources.len(), 1);
        assert!(state.native_err.is_none());
    }

    #[test]
    fn native_partial_error_applies_healthy_and_still_banners() {
        // One native runner failed to read; the poller drops it and sends the
        // healthy set with a banner. The app must apply the fresh healthy rows
        // (not freeze the whole slice) while surfacing the banner.
        let mut state = AppState {
            native_resources: vec![resource("stale-runner", SourceKind::Native)],
            ..Default::default()
        };

        apply_resource_update(
            &mut state,
            native_update(
                vec![resource("ltdovr", SourceKind::Native)],
                Some("native: scoop-vanscout: denied".to_string()),
            ),
        );

        // Healthy fresh row applied (old stale slice replaced), banner shown, and
        // the healthy runner's history recorded despite the error.
        assert_eq!(state.native_resources.len(), 1);
        assert_eq!(state.native_resources[0].name, "ltdovr");
        assert_eq!(
            state.native_err.as_deref(),
            Some("native: scoop-vanscout: denied")
        );
        assert!(!state.history.cpu("ltdovr").is_empty());
    }

    #[test]
    fn success_poll_records_history_for_both_sources() {
        let mut state = AppState::default();
        apply_resource_update(
            &mut state,
            docker_update(
                vec![resource("pulse-ci-runner-1", SourceKind::Docker)],
                None,
            ),
        );
        apply_resource_update(
            &mut state,
            native_update(vec![resource("ltdovr", SourceKind::Native)], None),
        );
        // Both runners now have history, and neither poll pruned the other's series.
        assert!(!state.history.cpu("pulse-ci-runner-1").is_empty());
        assert!(!state.history.cpu("ltdovr").is_empty());
    }

    #[test]
    fn error_poll_does_not_touch_history() {
        let mut state = AppState::default();
        apply_resource_update(
            &mut state,
            docker_update(
                vec![resource("pulse-ci-runner-1", SourceKind::Docker)],
                None,
            ),
        );
        let before = state.history.cpu("pulse-ci-runner-1").len();
        apply_resource_update(
            &mut state,
            docker_update(vec![], Some("docker: x".to_string())),
        );
        // The error poll neither appended a point nor cleared the series.
        assert_eq!(state.history.cpu("pulse-ci-runner-1").len(), before);
    }

    #[test]
    fn jobs_update_always_replaces() {
        // Preservation lives in the poller; the app just mirrors each update.
        let mut stale = HashMap::new();
        stale.insert(
            RunnerKey {
                scope: "erwins-enkel/pulse".into(),
                name: "runner-1".into(),
            },
            Some(job()),
        );
        let mut state = AppState {
            jobs: stale,
            jobs_err: Some("stale error".to_string()),
            ..Default::default()
        };

        let mut fresh = HashMap::new();
        fresh.insert(
            RunnerKey {
                scope: "erwins-enkel/pulse".into(),
                name: "runner-2".into(),
            },
            Some(job()),
        );
        apply_jobs_update(
            &mut state,
            JobsUpdate {
                jobs: fresh,
                hosted: Vec::new(),
                error: None,
            },
        );

        assert_eq!(state.jobs.len(), 1);
        assert!(state.jobs.contains_key(&RunnerKey {
            scope: "erwins-enkel/pulse".into(),
            name: "runner-2".into()
        }));
        assert!(state.jobs_err.is_none());
    }

    #[test]
    fn jobs_update_sets_hosted() {
        let mut state = AppState::default();
        let hosted = vec![HostedJob {
            repo: "o/r".into(),
            workflow: "CI".into(),
            job: "Build".into(),
            label: "ubuntu-latest".into(),
            branch: "main".into(),
            status: HostedStatus::InProgress,
            since: SystemTime::now(),
        }];
        apply_jobs_update(
            &mut state,
            JobsUpdate {
                jobs: HashMap::new(),
                hosted,
                error: None,
            },
        );
        assert_eq!(state.hosted.len(), 1);
        assert_eq!(state.hosted[0].job, "Build");
    }
}
