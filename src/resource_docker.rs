use crate::config::{Config, PrefixRule};
use crate::model::{runner_index, RunnerKey, RunnerResource, SourceKind};
use crate::resource::ResourceUpdate;
use crate::stats_math::{cpu_pct, mem_used};
use bollard::query_parameters::{ListContainersOptionsBuilder, StatsOptionsBuilder};
use bollard::Docker;
use futures_util::stream::TryStreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy)]
pub struct CpuSample {
    pub total: u64,
    pub system: u64,
}

/// The first rule (config order) whose prefix matches the container name
/// (leading `/` trimmed), or `None` when no rule matches.
pub fn matching_rule<'a>(name: &str, rules: &'a [PrefixRule]) -> Option<&'a PrefixRule> {
    let trimmed = name.trim_start_matches('/');
    rules.iter().find(|r| trimmed.starts_with(&r.prefix))
}

pub fn cpu_from_samples(prev: Option<CpuSample>, cur: CpuSample, online: u64) -> f64 {
    match prev {
        None => 0.0, // first poll: no prior snapshot to delta against
        Some(p) => cpu_pct(cur.total, p.total, cur.system, p.system, online),
    }
}

pub fn connect(socket_path: &str) -> anyhow::Result<Docker> {
    Ok(Docker::connect_with_unix(
        socket_path,
        120,
        bollard::API_DEFAULT_VERSION,
    )?)
}

/// The GitHub runner name for a pulse container: `pulse-ci-runner-4` → `runner-4`.
/// Reuses the one index path; an unparseable name falls back to the container
/// name (renders, just won't match a GitHub job → shows idle) rather than dropped.
fn docker_runner_name(container: &str) -> String {
    runner_index(container)
        .map(|i| format!("runner-{i}"))
        .unwrap_or_else(|| container.to_string())
}

pub async fn run(cfg: Config, tx: mpsc::Sender<ResourceUpdate>) {
    // Retained previous-poll CPU counters, keyed by container id. IGNORE the API's
    // precpu_stats (zeroed for one-shot stats); we compute the delta ourselves.
    let mut prev: HashMap<String, CpuSample> = HashMap::new();
    let mut docker: Option<Docker> = None;
    loop {
        if docker.is_none() {
            match connect(&cfg.socket_path) {
                Ok(d) => docker = Some(d),
                Err(e) => {
                    let _ = tx.send(err_update(format!("docker: {e}"))).await;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            }
        }
        let d = docker.as_ref().unwrap();
        match collect(d, &cfg.prefixes, &mut prev).await {
            Ok((resources, matched_seen, unmatched_seen)) => {
                let _ = tx
                    .send(ResourceUpdate {
                        source: SourceKind::Docker,
                        resources,
                        matched_seen,
                        unmatched_seen,
                        error: None,
                    })
                    .await;
            }
            Err(e) => {
                docker = None; // force reconnect next cycle
                let _ = tx.send(err_update(format!("docker: {e}"))).await;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn err_update(msg: String) -> ResourceUpdate {
    ResourceUpdate {
        source: SourceKind::Docker,
        resources: vec![],
        matched_seen: 0,
        unmatched_seen: 0,
        error: Some(msg),
    }
}

async fn collect(
    d: &Docker,
    rules: &[PrefixRule],
    prev: &mut HashMap<String, CpuSample>,
) -> anyhow::Result<(Vec<RunnerResource>, usize, usize)> {
    let list = d
        .list_containers(Some(ListContainersOptionsBuilder::default().build()))
        .await?;
    let total_seen = list.len();
    let mut matched_seen = 0;
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for c in list {
        let name = c
            .names
            .as_ref()
            .and_then(|n| n.first())
            .cloned()
            .unwrap_or_default();
        let repo = match matching_rule(&name, rules) {
            Some(rule) => rule.repo.as_deref(),
            None => continue,
        };
        matched_seen += 1;
        let id = match &c.id {
            Some(id) => id.clone(),
            None => continue,
        };
        seen.push(id.clone());
        if let Ok(Some(stat)) = d
            .stats(
                &id,
                Some(
                    StatsOptionsBuilder::default()
                        .stream(false)
                        .one_shot(true)
                        .build(),
                ),
            )
            .try_next()
            .await
        {
            if let Some(rr) = to_resource(&id, &name, repo, &stat, prev) {
                out.push(rr);
            }
        }
    }
    prev.retain(|k, _| seen.contains(k)); // drop deregistered containers
    Ok((out, matched_seen, total_seen - matched_seen))
}

/// `repo` is the matched rule's scope: `Some` maps the runner to a repo (job
/// detail via `join`), `None` leaves `key: None` so the row always renders idle.
fn to_resource(
    id: &str,
    name: &str,
    repo: Option<&str>,
    s: &bollard::models::ContainerStatsResponse,
    prev: &mut HashMap<String, CpuSample>,
) -> Option<RunnerResource> {
    let cpu = s.cpu_stats.as_ref()?;
    let mem = s.memory_stats.as_ref()?;
    let online = cpu.online_cpus.map(|v| v as u64).unwrap_or_else(|| {
        cpu.cpu_usage
            .as_ref()
            .and_then(|u| u.percpu_usage.as_ref())
            .map(|v| v.len() as u64)
            .unwrap_or(1)
    });
    let cur = CpuSample {
        total: cpu
            .cpu_usage
            .as_ref()
            .and_then(|u| u.total_usage)
            .unwrap_or(0),
        system: cpu.system_cpu_usage.unwrap_or(0),
    };
    let pct = cpu_from_samples(prev.get(id).copied(), cur, online);
    prev.insert(id.to_string(), cur);
    let inactive = mem
        .stats
        .as_ref()
        .and_then(|m| m.get("inactive_file").copied())
        .unwrap_or(0);
    let used = mem_used(mem.usage.unwrap_or(0), inactive);
    let display = name.trim_start_matches('/').to_string();
    Some(RunnerResource {
        key: repo.map(|scope| RunnerKey {
            scope: scope.to_string(),
            name: docker_runner_name(&display),
        }),
        name: display,
        cpu_pct: pct,
        mem_bytes: used,
        mem_limit: mem.limit.unwrap_or(0),
        kind: SourceKind::Docker,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(prefix: &str, repo: Option<&str>) -> PrefixRule {
        PrefixRule {
            prefix: prefix.into(),
            repo: repo.map(String::from),
        }
    }

    #[test]
    fn matching_rule_trims_leading_slash_and_first_match_wins() {
        let rules = vec![
            rule("pulse-ci-runner-", Some("erwins-enkel/pulse")),
            rule("pulse-", Some("other/repo")),
        ];
        // Leading `/` trimmed; the first rule that matches (config order) wins.
        assert_eq!(
            matching_rule("/pulse-ci-runner-4", &rules).map(|r| r.prefix.as_str()),
            Some("pulse-ci-runner-")
        );
        assert!(matching_rule("other-thing", &rules).is_none());
    }

    #[test]
    fn scope_selection_mapped_fleets_get_distinct_keys() {
        // Each mapped fleet resolves to its own repo; runner-name derivation is
        // identical (both -> runner-1) but the scope keeps the keys distinct.
        let rules = vec![
            rule("pulse-ci-runner-", Some("erwins-enkel/pulse")),
            rule("flowagent-ci-runner-", Some("ltdovr/flowagent")),
        ];
        let pulse = matching_rule("pulse-ci-runner-1", &rules).unwrap();
        let flow = matching_rule("flowagent-ci-runner-1", &rules).unwrap();
        assert_eq!(pulse.repo.as_deref(), Some("erwins-enkel/pulse"));
        assert_eq!(flow.repo.as_deref(), Some("ltdovr/flowagent"));
    }

    #[test]
    fn scope_selection_unmapped_second_fleet_yields_no_key() {
        // Post-`resolve` state of two unmapped prefixes with one configured repo:
        // only the first inherits it; the second stays None -> `key: None`, so it
        // cannot inherit the first fleet's runner-1 job.
        let rules = vec![
            rule("pulse-ci-runner-", Some("erwins-enkel/pulse")),
            rule("flowagent-ci-runner-", None),
        ];
        let flow = matching_rule("flowagent-ci-runner-1", &rules).unwrap();
        assert_eq!(flow.repo, None);
    }

    #[test]
    fn first_poll_zero_then_delta_from_retained_sample() {
        // First poll: no prior sample → 0% (cannot delta a single snapshot).
        let s0 = CpuSample {
            total: 1_000_000_000,
            system: 4_000_000_000,
        };
        assert_eq!(cpu_from_samples(None, s0, 4), 0.0);
        // Second poll: 1 full core used over the interval on a 4-core box → 100%.
        let s1 = CpuSample {
            total: 2_000_000_000,
            system: 8_000_000_000,
        };
        let pct = cpu_from_samples(Some(s0), s1, 4);
        assert!((pct - 100.0).abs() < 0.001, "got {pct}");
    }

    #[test]
    fn docker_runner_name_maps_container_to_github_runner() {
        // Container name → the GitHub runner_name the pulse repo's jobs report.
        assert_eq!(docker_runner_name("pulse-ci-runner-4"), "runner-4");
        // Unparseable trailing segment falls back to the container name.
        assert_eq!(docker_runner_name("weird-name"), "weird-name");
    }
}
