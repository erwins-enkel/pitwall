use crate::config::Config;
use crate::model::{sort_hosted, HostedJob, HostedStatus, JobInfo, RunnerKey};
use futures_util::{stream, StreamExt};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio::sync::mpsc;

/// Jobs keyed by scope-qualified [`RunnerKey`]. A present key means the runner
/// is busy: `Some(JobInfo)` carries workflow › job detail (repo scopes),
/// `None` is busy-without-detail (org scopes).
type Slice = HashMap<RunnerKey, Option<JobInfo>>;

/// Per-scope poll result: the self-hosted runner slice plus hosted jobs. Kept as
/// the last-known-good unit so a failed poll preserves both together.
#[derive(Default, Clone)]
pub struct ScopeState {
    pub slice: Slice,
    pub hosted: Vec<HostedJob>,
}

pub struct JobsUpdate {
    pub jobs: Slice,
    pub hosted: Vec<HostedJob>,
    pub error: Option<String>,
}

/// Extracts `(run id, workflow name, head branch)` per in-progress run. The
/// branch is the ref the run was started for; missing/empty is carried as "".
pub fn parse_runs(json: &str) -> Vec<(u64, String, String)> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("workflow_runs").and_then(|r| r.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| {
            let id = r.get("id")?.as_u64()?;
            let name = r
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let branch = r
                .get("head_branch")
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .to_string();
            Some((id, name, branch))
        })
        .collect()
}

fn parse_rfc3339(s: &str) -> SystemTime {
    humantime::parse_rfc3339(s).unwrap_or_else(|_| SystemTime::now())
}

/// In-progress jobs paired with their raw `runner_name` (the GitHub runner name,
/// e.g. `runner-4` for pulse or the `.runner` agentName for native runners).
/// GitHub-hosted jobs carry a `runner_name` that matches no self-hosted runner
/// key, so they're harmless — `model::join` only surfaces keys that match a row.
pub fn parse_jobs(workflow: &str, branch: &str, json: &str) -> Vec<(String, JobInfo)> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    v.get("jobs")
        .and_then(|j| j.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|j| j.get("status").and_then(|s| s.as_str()) == Some("in_progress"))
                .filter_map(|j| {
                    let runner_name = j.get("runner_name")?.as_str()?.to_string();
                    let job = j.get("name")?.as_str()?.to_string();
                    let started = j
                        .get("started_at")
                        .and_then(|s| s.as_str())
                        .map(parse_rfc3339)
                        .unwrap_or_else(SystemTime::now);
                    Some((
                        runner_name,
                        JobInfo {
                            workflow: workflow.to_string(),
                            job,
                            branch: branch.to_string(),
                            started_at: started,
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// True if a GitHub job's `labels` array marks it self-hosted. GitHub auto-adds
/// the `self-hosted` label to every self-hosted runner job; hosted jobs never
/// carry it. A missing/!array `labels` is treated as not-self-hosted (hosted).
fn is_self_hosted(labels: &serde_json::Value) -> bool {
    labels
        .as_array()
        .is_some_and(|arr| arr.iter().any(|l| l.as_str() == Some("self-hosted")))
}

/// Hosted (non-self-hosted) jobs in status `in_progress`/`queued` from a run's
/// jobs payload. `since` is `started_at` for running jobs, `created_at` for
/// queued. `label` is the first requested label (e.g. `ubuntu-latest`).
/// The hosted/self-hosted split is decided by [`is_self_hosted`] off the job's
/// `labels`, which just echo the workflow's `runs-on` request: a self-hosted
/// workflow whose `runs-on` is a bare custom label (no `self-hosted` in the
/// list) would also surface here as "hosted".
pub fn parse_hosted_jobs(workflow: &str, branch: &str, json: &str) -> Vec<HostedJob> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    v.get("jobs")
        .and_then(|j| j.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|j| {
                    let status = match j.get("status").and_then(|s| s.as_str())? {
                        "in_progress" => HostedStatus::InProgress,
                        "queued" => HostedStatus::Queued,
                        _ => return None,
                    };
                    let labels = j.get("labels").cloned().unwrap_or(serde_json::Value::Null);
                    if is_self_hosted(&labels) {
                        return None;
                    }
                    let job = j.get("name")?.as_str()?.to_string();
                    let label = labels
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(|l| l.as_str())
                        .unwrap_or("hosted")
                        .to_string();
                    let ts_key = match status {
                        HostedStatus::InProgress => "started_at",
                        HostedStatus::Queued => "created_at",
                    };
                    let since = j
                        .get(ts_key)
                        .and_then(|s| s.as_str())
                        .map(parse_rfc3339)
                        .unwrap_or_else(SystemTime::now);
                    Some(HostedJob {
                        workflow: workflow.to_string(),
                        job,
                        label,
                        branch: branch.to_string(),
                        status,
                        since,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Busy runner names from an org/repo runners-endpoint response.
pub fn parse_org_runners(json: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("runners").and_then(|r| r.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter(|r| r.get("busy").and_then(|b| b.as_bool()).unwrap_or(false))
        .filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect()
}

/// Per-scope poll outcome fed to [`merge_scopes`].
pub enum ScopeOutcome {
    /// Fresh data — replaces the scope's prior state (empty ⇒ clears it).
    Ok(ScopeState),
    /// A repo poll failed — keep the prior state AND flag the scope in the banner.
    RepoErr,
    /// An org poll failed/403 — keep the prior state, NEVER flag it (the box's
    /// org endpoint is permanently 403; it must not paint a permanent banner).
    OrgSkip,
}

/// Merge fresh per-scope outcomes into the previous per-scope snapshot.
/// Returns the new snapshot and, only if a *repo* scope failed, an error summary
/// naming the failed repos. Org failures are silent by construction.
pub fn merge_scopes(
    mut prev: HashMap<String, ScopeState>,
    results: Vec<(String, ScopeOutcome)>,
) -> (HashMap<String, ScopeState>, Option<String>) {
    let mut failed: Vec<String> = Vec::new();
    for (scope, outcome) in results {
        match outcome {
            ScopeOutcome::Ok(state) => {
                prev.insert(scope, state);
            }
            ScopeOutcome::RepoErr => failed.push(scope),
            ScopeOutcome::OrgSkip => {}
        }
    }
    let err = if failed.is_empty() {
        None
    } else {
        Some(format!("gh: {}", failed.join(", ")))
    };
    (prev, err)
}

fn flatten(per_scope: &HashMap<String, ScopeState>) -> (Slice, Vec<HostedJob>) {
    let mut slice = Slice::new();
    let mut hosted = Vec::new();
    for state in per_scope.values() {
        for (k, v) in &state.slice {
            slice.insert(k.clone(), v.clone());
        }
        hosted.extend(state.hosted.iter().cloned());
    }
    (slice, hosted)
}

async fn gh_api(path: &str) -> anyhow::Result<String> {
    let out = Command::new("gh").arg("api").arg(path).output().await?;
    if !out.status.success() {
        anyhow::bail!(
            "gh api failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn poll_repo(repo: &str) -> anyhow::Result<ScopeState> {
    let mut st = ScopeState::default();
    for status in ["in_progress", "queued"] {
        let runs_json = gh_api(&format!("repos/{repo}/actions/runs?status={status}")).await?;
        for (id, name, branch) in parse_runs(&runs_json) {
            let jobs_json = gh_api(&format!("repos/{repo}/actions/runs/{id}/jobs")).await?;
            for (runner_name, ji) in parse_jobs(&name, &branch, &jobs_json) {
                st.slice.insert(
                    RunnerKey {
                        scope: repo.to_string(),
                        name: runner_name,
                    },
                    Some(ji),
                );
            }
            st.hosted
                .extend(parse_hosted_jobs(&name, &branch, &jobs_json));
        }
    }
    Ok(st)
}

async fn poll_org(org: &str) -> Option<ScopeState> {
    // 403 (no admin:org) or any error → None → OrgSkip (silent, keep prior).
    let json = gh_api(&format!("orgs/{org}/actions/runners")).await.ok()?;
    let mut slice = Slice::new();
    for name in parse_org_runners(&json) {
        slice.insert(
            RunnerKey {
                scope: org.to_string(),
                name,
            },
            None,
        );
    }
    Some(ScopeState {
        slice,
        hosted: Vec::new(),
    })
}

/// Whether a scope is polled as a repo (job detail) or an org (busy only).
#[derive(Clone, Copy)]
enum ScopeKind {
    Repo,
    Org,
}

/// Poll one scope. Repo failures become `RepoErr` (banner); org failures become
/// `OrgSkip` (silent). A single async fn keeps every future the same concrete
/// type, so they can share one `buffer_unordered` stream without boxing.
async fn poll_scope(scope: String, kind: ScopeKind) -> (String, ScopeOutcome) {
    let outcome = match kind {
        ScopeKind::Repo => match poll_repo(&scope).await {
            Ok(s) => ScopeOutcome::Ok(s),
            Err(_) => ScopeOutcome::RepoErr,
        },
        ScopeKind::Org => match poll_org(&scope).await {
            Some(s) => ScopeOutcome::Ok(s),
            None => ScopeOutcome::OrgSkip,
        },
    };
    (scope, outcome)
}

pub async fn run(cfg: Config, tx: mpsc::Sender<JobsUpdate>) {
    let mut prev: HashMap<String, ScopeState> = HashMap::new();
    loop {
        if cfg.repos.is_empty() && cfg.orgs.is_empty() {
            // No configured repos and no native scopes → nothing pollable.
            let _ = tx
                .send(JobsUpdate {
                    jobs: Slice::new(),
                    hosted: Vec::new(),
                    error: Some(
                        "PITWALL_REPO unset — set it to your runners' repo (e.g. myorg/myrepo)"
                            .into(),
                    ),
                })
                .await;
            tokio::time::sleep(Duration::from_secs(15)).await;
            continue;
        }

        let repo_futs = cfg
            .repos
            .clone()
            .into_iter()
            .map(|scope| poll_scope(scope, ScopeKind::Repo));
        let org_futs = cfg
            .orgs
            .clone()
            .into_iter()
            .map(|scope| poll_scope(scope, ScopeKind::Org));
        // Poll scopes concurrently (bounded) so total wall time ≈ slowest scope.
        let results: Vec<(String, ScopeOutcome)> = stream::iter(repo_futs.chain(org_futs))
            .buffer_unordered(6)
            .collect()
            .await;

        let (next, error) = merge_scopes(std::mem::take(&mut prev), results);
        prev = next;
        let (jobs, mut hosted) = flatten(&prev);
        sort_hosted(&mut hosted, SystemTime::now());
        let _ = tx
            .send(JobsUpdate {
                jobs,
                hosted,
                error,
            })
            .await;
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HostedStatus;

    fn state_with(scope: &str, name: &str) -> ScopeState {
        let mut s = Slice::new();
        s.insert(
            RunnerKey {
                scope: scope.into(),
                name: name.into(),
            },
            Some(JobInfo {
                workflow: "w".into(),
                job: "j".into(),
                branch: "main".into(),
                started_at: SystemTime::now(),
            }),
        );
        ScopeState {
            slice: s,
            hosted: Vec::new(),
        }
    }

    #[test]
    fn parse_runs_extracts_id_name_and_branch() {
        let runs = parse_runs(include_str!("../tests/fixtures/runs.json"));
        assert_eq!(runs, vec![(123u64, "Test".to_string(), "main".to_string())]);
    }

    #[test]
    fn parse_jobs_keeps_in_progress_drops_completed() {
        let out = parse_jobs("Test", "main", include_str!("../tests/fixtures/jobs.json"));
        // Both in_progress jobs survive (self-hosted runner-4 + a hosted one);
        // completed jobs are dropped. The hosted job is harmless — it matches no
        // runner key in `join`.
        assert_eq!(out.len(), 2);
        let runner4 = out.iter().find(|(rn, _)| rn == "runner-4").unwrap();
        assert_eq!(runner4.1.job, "E2E Tests");
        assert_eq!(runner4.1.branch, "main");
        assert!(out.iter().all(|(_, ji)| ji.job != "Coverage Gate"));
    }

    #[test]
    fn parse_jobs_passes_through_native_agent_name() {
        // Native runner_name equals the .runner agentName (verified live).
        let out = parse_jobs(
            "Deploy",
            "dev",
            include_str!("../tests/fixtures/native_jobs.json"),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "backontop-vanscout");
        assert_eq!(out[0].1.job, "Build");
        assert_eq!(out[0].1.branch, "dev");
    }

    #[test]
    fn parse_org_runners_returns_busy_names_only() {
        let names = parse_org_runners(include_str!("../tests/fixtures/org_runners.json"));
        assert_eq!(names, vec!["backontop"]);
    }

    #[test]
    fn merge_repo_failure_preserves_prior_and_names_scope() {
        let mut prev = HashMap::new();
        prev.insert(
            "scoop/vanscout".to_string(),
            state_with("scoop/vanscout", "backontop-vanscout"),
        );
        prev.insert(
            "scoop/kanban-api".to_string(),
            state_with("scoop/kanban-api", "backontop-kanban-api"),
        );

        // vanscout errors (keep prior + banner); kanban-api succeeds empty (clears).
        let results = vec![
            ("scoop/vanscout".to_string(), ScopeOutcome::RepoErr),
            (
                "scoop/kanban-api".to_string(),
                ScopeOutcome::Ok(ScopeState::default()),
            ),
        ];
        let (next, err) = merge_scopes(prev, results);

        // vanscout's prior rows persist; kanban-api cleared.
        assert!(next["scoop/vanscout"].slice.contains_key(&RunnerKey {
            scope: "scoop/vanscout".into(),
            name: "backontop-vanscout".into()
        }));
        assert!(next["scoop/kanban-api"].slice.is_empty());
        assert_eq!(err.as_deref(), Some("gh: scoop/vanscout"));
    }

    #[test]
    fn merge_org_failure_is_silent_and_preserves() {
        let mut prev = HashMap::new();
        let mut org_slice = Slice::new();
        org_slice.insert(
            RunnerKey {
                scope: "ltdovr".into(),
                name: "backontop".into(),
            },
            None,
        );
        prev.insert(
            "ltdovr".to_string(),
            ScopeState {
                slice: org_slice,
                hosted: vec![],
            },
        );

        let (next, err) = merge_scopes(prev, vec![("ltdovr".to_string(), ScopeOutcome::OrgSkip)]);

        // No banner from the permanent org 403; prior org busy state preserved.
        assert!(err.is_none());
        assert!(next["ltdovr"].slice.contains_key(&RunnerKey {
            scope: "ltdovr".into(),
            name: "backontop".into()
        }));
    }

    #[test]
    fn merge_flatten_unions_all_scopes() {
        let mut prev = HashMap::new();
        prev.insert(
            "scoop/vanscout".to_string(),
            state_with("scoop/vanscout", "backontop-vanscout"),
        );
        let mut org_slice = Slice::new();
        org_slice.insert(
            RunnerKey {
                scope: "ltdovr".into(),
                name: "backontop".into(),
            },
            None,
        );
        prev.insert(
            "ltdovr".to_string(),
            ScopeState {
                slice: org_slice,
                hosted: vec![],
            },
        );
        let (flat, _hosted) = flatten(&prev);
        assert_eq!(flat.len(), 2);
    }

    fn scope_state_with_hosted(job: &str) -> ScopeState {
        ScopeState {
            slice: Slice::new(),
            hosted: vec![HostedJob {
                workflow: "w".into(),
                job: job.into(),
                label: "ubuntu-latest".into(),
                branch: "main".into(),
                status: HostedStatus::InProgress,
                since: SystemTime::now(),
            }],
        }
    }

    #[test]
    fn merge_repo_error_preserves_prior_hosted() {
        let mut prev = HashMap::new();
        prev.insert("o/r".to_string(), scope_state_with_hosted("Build"));

        // Repo poll fails → keep prior scope state (hosted included) + banner.
        let (next, err) = merge_scopes(prev, vec![("o/r".to_string(), ScopeOutcome::RepoErr)]);

        assert_eq!(next["o/r"].hosted.len(), 1);
        assert_eq!(next["o/r"].hosted[0].job, "Build");
        assert_eq!(err.as_deref(), Some("gh: o/r"));
    }

    #[test]
    fn parse_hosted_jobs_keeps_hosted_running_and_queued_only() {
        let out = parse_hosted_jobs(
            "CI",
            "main",
            include_str!("../tests/fixtures/hosted_jobs.json"),
        );
        // self-hosted (E2E Tests) excluded; completed (Old Job) excluded.
        assert_eq!(out.len(), 2);

        let build = out.iter().find(|h| h.job == "Build").unwrap();
        assert_eq!(build.workflow, "CI");
        assert_eq!(build.branch, "main");
        assert_eq!(build.label, "ubuntu-latest");
        assert_eq!(build.status, HostedStatus::InProgress);
        // running → since == started_at (12:26:00Z)
        assert_eq!(build.since, parse_rfc3339("2026-07-04T12:26:00Z"));

        let lint = out.iter().find(|h| h.job == "Lint").unwrap();
        assert_eq!(lint.label, "ubuntu-24.04");
        assert_eq!(lint.status, HostedStatus::Queued);
        // queued → since == created_at (12:26:30Z)
        assert_eq!(lint.since, parse_rfc3339("2026-07-04T12:26:30Z"));
    }
}
