use crate::config::Config;
use crate::model::{sort_deployments, DeployStatus, Deployment};
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Result of a Vercel poll. No error field: Vercel errors degrade to an empty
/// list silently (auto-detect design — no status banner), unlike `JobsUpdate`
/// which carries `error`.
pub struct VercelUpdate {
    pub deployments: Vec<Deployment>,
}

#[derive(Debug, Deserialize)]
struct RawList {
    deployments: Vec<RawDeployment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDeployment {
    name: String,
    state: String,
    target: Option<String>,
    created_at: u64,
    building_at: Option<u64>,
    meta: Option<RawMeta>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMeta {
    github_commit_message: Option<String>,
    github_commit_ref: Option<String>,
    github_org: Option<String>,
    github_repo: Option<String>,
}

fn from_epoch_ms(n: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(n)
}

/// Parse a `vercel list --format json` payload into `Deployment`s, keeping only
/// in-flight (BUILDING/QUEUED) deployments whose `owner/repo` (from
/// `meta.githubOrg`/`meta.githubRepo`) matches one of `configured_repos`
/// (ASCII case-insensitive). Pure — no I/O. Malformed JSON or an unknown
/// `state` yields no element rather than a panic.
pub fn parse_deployments(json: &str, configured_repos: &[String]) -> Vec<Deployment> {
    let raw: RawList = match serde_json::from_str(json) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    raw.deployments
        .into_iter()
        .filter_map(|d| {
            let status = match d.state.as_str() {
                "BUILDING" => DeployStatus::Building,
                "QUEUED" => DeployStatus::Queued,
                _ => return None,
            };
            let meta = d.meta?;
            let org = meta.github_org?;
            let repo_name = meta.github_repo?;
            let repo = format!("{org}/{repo_name}");
            if !configured_repos
                .iter()
                .any(|r| r.eq_ignore_ascii_case(&repo))
            {
                return None;
            }
            let started_at = match status {
                DeployStatus::Building => from_epoch_ms(d.building_at.unwrap_or(d.created_at)),
                DeployStatus::Queued => from_epoch_ms(d.created_at),
            };
            let target = d.target.unwrap_or_else(|| "preview".to_string());
            let branch = meta.github_commit_ref.unwrap_or_default();
            let commit_summary = meta
                .github_commit_message
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            Some(Deployment {
                repo,
                project: d.name,
                target,
                branch,
                commit_summary,
                status,
                started_at,
            })
        })
        .collect()
}

async fn vercel_list() -> anyhow::Result<String> {
    let out = timeout(
        Duration::from_secs(10),
        Command::new("vercel")
            .args(["list", "--format", "json", "--status", "BUILDING,QUEUED"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("vercel list timed out"))??;
    if !out.status.success() {
        anyhow::bail!(
            "vercel list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub async fn run(cfg: Config, tx: mpsc::Sender<VercelUpdate>) {
    loop {
        let deployments = if cfg.configured_repos.is_empty() {
            Vec::new()
        } else {
            match vercel_list().await {
                Ok(json) => {
                    let mut d = parse_deployments(&json, &cfg.configured_repos);
                    sort_deployments(&mut d, SystemTime::now());
                    d
                }
                Err(_) => Vec::new(),
            }
        };
        if tx.send(VercelUpdate { deployments }).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
    {
      "deployments": [
        {
          "url": "pulse-abc-zweizeiler.vercel.app",
          "name": "pulse",
          "state": "BUILDING",
          "target": "production",
          "createdAt": 1783260670752,
          "buildingAt": 1783260672238,
          "meta": {
            "githubCommitMessage": "fix: something important\n\nbody line here",
            "githubCommitRef": "main",
            "githubOrg": "erwins-enkel",
            "githubRepo": "pulse"
          }
        },
        {
          "url": "pulse-def-zweizeiler.vercel.app",
          "name": "pulse",
          "state": "QUEUED",
          "target": null,
          "createdAt": 1783260680000,
          "meta": {
            "githubCommitMessage": "feat: queued change",
            "githubCommitRef": "feat/x",
            "githubOrg": "erwins-enkel",
            "githubRepo": "pulse"
          }
        },
        {
          "url": "shepherd-site-xyz-zweizeiler.vercel.app",
          "name": "shepherd-site",
          "state": "BUILDING",
          "target": "production",
          "createdAt": 1783260690000,
          "buildingAt": 1783260691000,
          "meta": {
            "githubCommitMessage": "chore: unrelated",
            "githubCommitRef": "main",
            "githubOrg": "erwins-enkel",
            "githubRepo": "shepherd"
          }
        },
        {
          "url": "nometa-zweizeiler.vercel.app",
          "name": "mystery",
          "state": "BUILDING",
          "target": "production",
          "createdAt": 1783260695000,
          "buildingAt": 1783260696000
        }
      ]
    }
    "#;

    #[test]
    fn filters_to_configured_repo_only() {
        let repos = vec!["erwins-enkel/pulse".to_string()];
        let out = parse_deployments(FIXTURE, &repos);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|d| d.project == "pulse"));
    }

    #[test]
    fn derives_status_and_started_at() {
        let repos = vec!["erwins-enkel/pulse".to_string()];
        let out = parse_deployments(FIXTURE, &repos);
        let building = out
            .iter()
            .find(|d| d.status == DeployStatus::Building)
            .unwrap();
        assert_eq!(
            building.started_at,
            UNIX_EPOCH + Duration::from_millis(1783260672238)
        );
        let queued = out
            .iter()
            .find(|d| d.status == DeployStatus::Queued)
            .unwrap();
        assert_eq!(
            queued.started_at,
            UNIX_EPOCH + Duration::from_millis(1783260680000)
        );
    }

    #[test]
    fn maps_fields_for_queued_deployment() {
        let repos = vec!["erwins-enkel/pulse".to_string()];
        let out = parse_deployments(FIXTURE, &repos);
        let queued = out
            .iter()
            .find(|d| d.status == DeployStatus::Queued)
            .unwrap();
        assert_eq!(queued.target, "preview");
        assert_eq!(queued.branch, "feat/x");
        assert_eq!(queued.commit_summary, "feat: queued change");
        assert_eq!(queued.repo, "erwins-enkel/pulse");
        assert_eq!(queued.project, "pulse");
    }

    #[test]
    fn matches_repo_case_insensitively() {
        let repos = vec!["erwins-enkel/PULSE".to_string()];
        let out = parse_deployments(FIXTURE, &repos);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn drops_unknown_state() {
        let json = r#"
        {
          "deployments": [
            {
              "name": "pulse",
              "state": "READY",
              "target": "production",
              "createdAt": 1783260670752,
              "meta": {
                "githubCommitMessage": "x",
                "githubCommitRef": "main",
                "githubOrg": "erwins-enkel",
                "githubRepo": "pulse"
              }
            }
          ]
        }
        "#;
        let out = parse_deployments(json, &["erwins-enkel/pulse".to_string()]);
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn malformed_json_returns_empty_vec() {
        let out = parse_deployments("not json", &["a/b".to_string()]);
        assert!(out.is_empty());
    }
}
