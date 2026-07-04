use crate::config::Config;
use crate::model::JobInfo;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tokio::process::Command;
use tokio::sync::mpsc;

pub struct JobsUpdate {
    pub jobs: HashMap<u32, JobInfo>,
    pub error: Option<String>,
}

pub fn parse_runs(json: &str) -> Vec<(u64, String)> {
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
            Some((id, name))
        })
        .collect()
}

fn parse_rfc3339(s: &str) -> SystemTime {
    humantime::parse_rfc3339(s).unwrap_or_else(|_| SystemTime::now())
}

/// Strict self-hosted key: `runner-<digits>` only. Rejects "GitHub Actions 123", "gh-runner-3", etc.
fn runner_index_strict(runner_name: &str) -> Option<u32> {
    let n = runner_name.strip_prefix("runner-")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

pub fn parse_jobs(workflow: &str, json: &str) -> Vec<(u32, JobInfo)> {
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
                    let rn = j.get("runner_name")?.as_str()?;
                    let idx = runner_index_strict(rn)?;
                    let job = j.get("name")?.as_str()?.to_string();
                    let started = j
                        .get("started_at")
                        .and_then(|s| s.as_str())
                        .map(parse_rfc3339)
                        .unwrap_or_else(SystemTime::now);
                    Some((
                        idx,
                        JobInfo {
                            workflow: workflow.to_string(),
                            job,
                            started_at: started,
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
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

async fn poll(repo: &str) -> anyhow::Result<HashMap<u32, JobInfo>> {
    let runs_json = gh_api(&format!("repos/{repo}/actions/runs?status=in_progress")).await?;
    let mut map = HashMap::new();
    for (id, name) in parse_runs(&runs_json) {
        let jobs_json = gh_api(&format!("repos/{repo}/actions/runs/{id}/jobs")).await?;
        for (idx, ji) in parse_jobs(&name, &jobs_json) {
            map.insert(idx, ji);
        }
    }
    Ok(map)
}

pub async fn run(cfg: Config, tx: mpsc::Sender<JobsUpdate>) {
    loop {
        let update = match poll(&cfg.repo).await {
            Ok(jobs) => JobsUpdate { jobs, error: None },
            Err(e) => JobsUpdate {
                jobs: HashMap::new(),
                error: Some(format!("gh: {e}")),
            },
        };
        let _ = tx.send(update).await;
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_runs_extracts_id_and_name() {
        let runs = parse_runs(include_str!("../tests/fixtures/runs.json"));
        assert_eq!(runs, vec![(123u64, "Test".to_string())]);
    }

    #[test]
    fn parse_jobs_keeps_only_in_progress_self_hosted() {
        let out = parse_jobs("Test", include_str!("../tests/fixtures/jobs.json"));
        // Only the in_progress runner-4 job survives; hosted + completed excluded.
        assert_eq!(out.len(), 1);
        let (idx, ji) = &out[0];
        assert_eq!(*idx, 4);
        assert_eq!(ji.workflow, "Test");
        assert_eq!(ji.job, "E2E Tests");
    }
}
