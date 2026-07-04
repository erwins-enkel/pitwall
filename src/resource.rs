use crate::config::Config;
use crate::model::RunnerResource;
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

pub struct ResourceUpdate {
    pub resources: Vec<RunnerResource>,
    /// Running containers whose name matched the prefix this poll. Non-zero with
    /// empty `resources` means matches exist but their stats weren't ready —
    /// distinct from a prefix mismatch.
    pub matched_seen: usize,
    /// Running containers whose name did NOT match the prefix. Drives the
    /// "N running, none match the prefix" hint when nothing matched.
    pub unmatched_seen: usize,
    pub error: Option<String>,
}

pub fn container_matches(name: &str, prefix: &str) -> bool {
    name.trim_start_matches('/').starts_with(prefix)
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
                    let _ = tx
                        .send(ResourceUpdate {
                            resources: vec![],
                            matched_seen: 0,
                            unmatched_seen: 0,
                            error: Some(format!("docker: {e}")),
                        })
                        .await;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            }
        }
        let d = docker.as_ref().unwrap();
        match collect(d, &cfg.prefix, &mut prev).await {
            Ok((resources, matched_seen, unmatched_seen)) => {
                let _ = tx
                    .send(ResourceUpdate {
                        resources,
                        matched_seen,
                        unmatched_seen,
                        error: None,
                    })
                    .await;
            }
            Err(e) => {
                docker = None; // force reconnect next cycle
                let _ = tx
                    .send(ResourceUpdate {
                        resources: vec![],
                        matched_seen: 0,
                        unmatched_seen: 0,
                        error: Some(format!("docker: {e}")),
                    })
                    .await;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn collect(
    d: &Docker,
    prefix: &str,
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
        if !container_matches(&name, prefix) {
            continue;
        }
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
            if let Some(rr) = to_resource(&id, &name, &stat, prev) {
                out.push(rr);
            }
        }
    }
    prev.retain(|k, _| seen.contains(k)); // drop deregistered containers
    Ok((out, matched_seen, total_seen - matched_seen))
}

fn to_resource(
    id: &str,
    name: &str,
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
    Some(RunnerResource {
        name: name.trim_start_matches('/').to_string(),
        cpu_pct: pct,
        mem_bytes: used,
        mem_limit: mem.limit.unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_prefix_ignoring_leading_slash() {
        assert!(container_matches("/ci-runner-4", "ci-runner-"));
        assert!(container_matches("ci-runner-1", "ci-runner-"));
        assert!(!container_matches("other-thing", "ci-runner-"));
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
}
