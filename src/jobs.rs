use crate::config::{Config, DEFAULT_REPO};
use crate::model::{JobInfo, RunnerKey};
use futures_util::{stream, StreamExt};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio::sync::mpsc;

/// Jobs keyed by scope-qualified [`RunnerKey`]. A present key means the runner
/// is busy: `Some(JobInfo)` carries workflow › job detail (repo scopes),
/// `None` is busy-without-detail (org scopes).
type Slice = HashMap<RunnerKey, Option<JobInfo>>;

pub struct JobsUpdate {
    pub jobs: Slice,
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
    /// Fresh data — replaces the scope's prior slice (empty ⇒ clears it).
    Ok(Slice),
    /// A repo poll failed — keep the prior slice AND flag the scope in the banner.
    RepoErr,
    /// An org poll failed/403 — keep the prior slice, NEVER flag it (the box's
    /// org endpoint is permanently 403; it must not paint a permanent banner).
    OrgSkip,
}

/// Merge fresh per-scope outcomes into the previous per-scope snapshot.
/// Returns the new snapshot and, only if a *repo* scope failed, an error summary
/// naming the failed repos. Org failures are silent by construction.
pub fn merge_scopes(
    mut prev: HashMap<String, Slice>,
    results: Vec<(String, ScopeOutcome)>,
) -> (HashMap<String, Slice>, Option<String>) {
    let mut failed: Vec<String> = Vec::new();
    for (scope, outcome) in results {
        match outcome {
            ScopeOutcome::Ok(slice) => {
                prev.insert(scope, slice);
            }
            ScopeOutcome::RepoErr => {
                failed.push(scope); // keep prior slice
            }
            ScopeOutcome::OrgSkip => { /* keep prior slice, no banner */ }
        }
    }
    let err = if failed.is_empty() {
        None
    } else {
        Some(format!("gh: {}", failed.join(", ")))
    };
    (prev, err)
}

fn flatten(per_scope: &HashMap<String, Slice>) -> Slice {
    let mut out = HashMap::new();
    for slice in per_scope.values() {
        for (k, v) in slice {
            out.insert(k.clone(), v.clone());
        }
    }
    out
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

async fn poll_repo(repo: &str) -> anyhow::Result<Slice> {
    let runs_json = gh_api(&format!("repos/{repo}/actions/runs?status=in_progress")).await?;
    let mut slice = Slice::new();
    for (id, name, branch) in parse_runs(&runs_json) {
        let jobs_json = gh_api(&format!("repos/{repo}/actions/runs/{id}/jobs")).await?;
        for (runner_name, ji) in parse_jobs(&name, &branch, &jobs_json) {
            slice.insert(
                RunnerKey {
                    scope: repo.to_string(),
                    name: runner_name,
                },
                Some(ji),
            );
        }
    }
    Ok(slice)
}

async fn poll_org(org: &str) -> Option<Slice> {
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
    Some(slice)
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
    let mut prev: HashMap<String, Slice> = HashMap::new();
    loop {
        // The unset-PITWALL_REPO sentinel can't be polled; skip it. Native
        // runners bring their own real scopes, so it's just excluded, not fatal.
        let real_repos: Vec<String> = cfg
            .repos
            .iter()
            .filter(|r| r.as_str() != DEFAULT_REPO)
            .cloned()
            .collect();

        if real_repos.is_empty() && cfg.orgs.is_empty() {
            // Nothing pollable: the only repo is the unset sentinel and there are
            // no native scopes. Surface the config hint instead of polling gh.
            let _ = tx
                .send(JobsUpdate {
                    jobs: Slice::new(),
                    error: Some(
                        "PITWALL_REPO unset — set it to your runners' repo (e.g. myorg/myrepo)"
                            .into(),
                    ),
                })
                .await;
            tokio::time::sleep(Duration::from_secs(15)).await;
            continue;
        }

        let repo_futs = real_repos
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
        let _ = tx
            .send(JobsUpdate {
                jobs: flatten(&prev),
                error,
            })
            .await;
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slice_with(scope: &str, name: &str) -> Slice {
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
        s
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
            slice_with("scoop/vanscout", "backontop-vanscout"),
        );
        prev.insert(
            "scoop/kanban-api".to_string(),
            slice_with("scoop/kanban-api", "backontop-kanban-api"),
        );

        // vanscout errors (keep prior + banner); kanban-api succeeds empty (clears).
        let results = vec![
            ("scoop/vanscout".to_string(), ScopeOutcome::RepoErr),
            (
                "scoop/kanban-api".to_string(),
                ScopeOutcome::Ok(Slice::new()),
            ),
        ];
        let (next, err) = merge_scopes(prev, results);

        // vanscout's prior rows persist; kanban-api cleared.
        assert!(next["scoop/vanscout"].contains_key(&RunnerKey {
            scope: "scoop/vanscout".into(),
            name: "backontop-vanscout".into()
        }));
        assert!(next["scoop/kanban-api"].is_empty());
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
        prev.insert("ltdovr".to_string(), org_slice);

        let (next, err) = merge_scopes(prev, vec![("ltdovr".to_string(), ScopeOutcome::OrgSkip)]);

        // No banner from the permanent org 403; prior org busy state preserved.
        assert!(err.is_none());
        assert!(next["ltdovr"].contains_key(&RunnerKey {
            scope: "ltdovr".into(),
            name: "backontop".into()
        }));
    }

    #[test]
    fn merge_flatten_unions_all_scopes() {
        let mut prev = HashMap::new();
        prev.insert(
            "scoop/vanscout".to_string(),
            slice_with("scoop/vanscout", "backontop-vanscout"),
        );
        let mut org_slice = Slice::new();
        org_slice.insert(
            RunnerKey {
                scope: "ltdovr".into(),
                name: "backontop".into(),
            },
            None,
        );
        prev.insert("ltdovr".to_string(), org_slice);
        let flat = flatten(&prev);
        assert_eq!(flat.len(), 2);
    }
}
