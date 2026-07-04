use crate::model::{RunnerKey, RunnerResource, SourceKind};
use crate::resource::ResourceUpdate;
use crate::stats_math::{cgroup_cpu_pct, mem_used};
use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// GitHub scope a native runner is registered against: a specific `owner/repo`
/// or a whole `owner` org. The string form (`as_key`) is what pairs with a
/// job's polling scope in [`RunnerKey`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Repo(String),
    Org(String),
}

impl Scope {
    pub fn as_key(&self) -> &str {
        match self {
            Scope::Repo(s) | Scope::Org(s) => s,
        }
    }
}

/// A discovered native runner: where to read its cgroup stats, and its GitHub
/// identity for job matching. `scope`/`key` are `None` when `.runner` couldn't
/// be read (resource-only row — shows CPU/mem, always idle).
#[derive(Debug, Clone)]
pub struct NativeRunner {
    pub display: String,
    pub cgroup_path: String,
    pub scope: Option<Scope>,
    pub key: Option<RunnerKey>,
}

/// Build the jobs poll-lists from the pulse repo + discovered native scopes.
/// `repos` = order-preserving dedup of `[pulse_repo]` + native repo-scopes (so
/// an overlapping repo isn't polled twice); `orgs` = distinct native org-scopes.
pub fn derive_scopes(pulse_repo: &str, runners: &[NativeRunner]) -> (Vec<String>, Vec<String>) {
    let mut repos = vec![pulse_repo.to_string()];
    let mut orgs: Vec<String> = Vec::new();
    let push_unique = |v: &mut Vec<String>, s: &str| {
        if !v.iter().any(|e| e == s) {
            v.push(s.to_string());
        }
    };
    for r in runners {
        match &r.scope {
            Some(Scope::Repo(s)) => push_unique(&mut repos, s),
            Some(Scope::Org(s)) => push_unique(&mut orgs, s),
            None => {}
        }
    }
    (repos, orgs)
}

fn run_systemctl(args: &[&str]) -> Option<String> {
    let out = Command::new("systemctl").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Enumerate `actions.runner.*.service` units and resolve each to a
/// [`NativeRunner`]. Returns an empty Vec when `systemctl` is absent, errors, or
/// lists no units (the normal off-box / CI case) — never panics.
pub fn discover() -> Vec<NativeRunner> {
    discover_inner(run_systemctl, |p| std::fs::read_to_string(p).ok())
}

fn discover_inner(
    systemctl: impl Fn(&[&str]) -> Option<String>,
    read_file: impl Fn(&str) -> Option<String>,
) -> Vec<NativeRunner> {
    let list = match systemctl(&[
        "list-units",
        "--type=service",
        "--all",
        "--no-legend",
        "--plain",
        "actions.runner.*.service",
    ]) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    for unit in parse_units_output(&list) {
        let show = match systemctl(&[
            "show",
            &unit,
            "-p",
            "ControlGroup",
            "-p",
            "WorkingDirectory",
        ]) {
            Some(s) => s,
            None => continue,
        };
        let (cgroup, workdir) = parse_show(&show);
        let cgroup = match cgroup {
            Some(c) if !c.is_empty() => c,
            _ => continue, // no cgroup → nothing to read
        };
        let cgroup_path = format!("/sys/fs/cgroup{cgroup}");
        let dot = workdir.and_then(|w| read_file(&format!("{w}/.runner")));
        if let Some(r) = build_runner(&unit, cgroup_path, dot.as_deref()) {
            out.push(r);
        }
    }
    out
}

/// Assemble a runner from already-fetched strings (pure). Unreadable/unparseable
/// `.runner` (`dot_runner` None or bad) yields a resource-only runner (`key`/
/// `scope` None) rather than dropping it.
fn build_runner(unit: &str, cgroup_path: String, dot_runner: Option<&str>) -> Option<NativeRunner> {
    let display = unit_display(unit)?;
    let (scope, key) = match dot_runner.and_then(parse_dot_runner) {
        Some((agent, scope)) => {
            let key = RunnerKey {
                scope: scope.as_key().to_string(),
                name: agent,
            };
            (Some(scope), Some(key))
        }
        None => (None, None),
    };
    Some(NativeRunner {
        display,
        cgroup_path,
        scope,
        key,
    })
}

fn parse_units_output(s: &str) -> Vec<String> {
    s.lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|t| t.starts_with("actions.runner.") && t.ends_with(".service"))
        .map(String::from)
        .collect()
}

fn parse_show(s: &str) -> (Option<String>, Option<String>) {
    let mut cgroup = None;
    let mut workdir = None;
    for l in s.lines() {
        if let Some(v) = l.strip_prefix("ControlGroup=") {
            cgroup = Some(v.trim().to_string());
        } else if let Some(v) = l.strip_prefix("WorkingDirectory=") {
            workdir = Some(v.trim().to_string());
        }
    }
    (cgroup, workdir)
}

/// `actions.runner.scoop-kanban-api.backontop-kanban-api.service` →
/// `scoop-kanban-api` (the registration segment before the host — unique even
/// when agentName collides).
fn unit_display(unit: &str) -> Option<String> {
    let stem = unit
        .strip_prefix("actions.runner.")?
        .strip_suffix(".service")?;
    Some(
        stem.rsplit_once('.')
            .map(|(a, _)| a.to_string())
            .unwrap_or_else(|| stem.to_string()),
    )
}

fn parse_scope(url: &str) -> Option<Scope> {
    let rest = url
        .trim()
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")
        .or_else(|| {
            url.trim()
                .trim_end_matches('/')
                .strip_prefix("http://github.com/")
        })?;
    let mut segs = rest.split('/').filter(|s| !s.is_empty());
    let owner = segs.next()?;
    match segs.next() {
        Some(repo) => Some(Scope::Repo(format!("{owner}/{repo}"))),
        None => Some(Scope::Org(owner.to_string())),
    }
}

fn parse_dot_runner(s: &str) -> Option<(String, Scope)> {
    let s = s.trim_start_matches('\u{feff}'); // .runner ships with a UTF-8 BOM
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let agent = v.get("agentName")?.as_str()?.to_string();
    let scope = parse_scope(v.get("gitHubUrl")?.as_str()?)?;
    Some((agent, scope))
}

fn parse_usage_usec(cpu_stat: &str) -> Option<u64> {
    cpu_stat
        .lines()
        .find_map(|l| l.strip_prefix("usage_usec "))
        .and_then(|v| v.trim().parse().ok())
}

fn parse_inactive_file(memory_stat: &str) -> u64 {
    memory_stat
        .lines()
        .find_map(|l| l.strip_prefix("inactive_file "))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

fn parse_mem_max(memory_max: &str) -> u64 {
    match memory_max.trim() {
        "max" => 0, // uncapped → no finite limit (never near-cap)
        n => n.parse().unwrap_or(0),
    }
}

/// Outcome of reading one runner's cgroup stats. Distinguishes a stopped unit
/// (`Gone`, dropped silently) from a genuine read failure (`Err`, surfaced as a
/// banner) so neither aborts the whole sweep.
enum ReadOutcome {
    /// A fresh reading.
    Ok(RunnerResource),
    /// The cgroup files are gone (`ENOENT`) — the unit stopped. Dropped silently
    /// this cycle, like an ephemeral docker container that deregistered.
    Gone,
    /// A genuine read error (e.g. permissions) — surfaced as a banner while the
    /// app keeps the last-known-good slice.
    Err(String),
}

/// Read one cgroup file. `Ok(None)` means absent (`ENOENT`); any other error is
/// a real failure carrying its message.
fn read_cgroup(path: &str) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Read one runner's cgroup stats into a [`ReadOutcome`]. Memory is the working
/// set (`memory.current` − `inactive_file`), matching the docker path.
fn collect_one(r: &NativeRunner, prev: &mut HashMap<String, (u64, Instant)>) -> ReadOutcome {
    // cpu.stat and memory.current are essential; their absence means the unit
    // stopped (Gone), any other read error is a real failure (Err).
    let cpu_stat = match read_cgroup(&format!("{}/cpu.stat", r.cgroup_path)) {
        Ok(Some(s)) => s,
        Ok(None) => return ReadOutcome::Gone,
        Err(e) => return ReadOutcome::Err(format!("{}: {e}", r.display)),
    };
    let usage = match parse_usage_usec(&cpu_stat) {
        Some(u) => u,
        None => return ReadOutcome::Err(format!("{}: cpu.stat has no usage_usec", r.display)),
    };
    let current: u64 = match read_cgroup(&format!("{}/memory.current", r.cgroup_path)) {
        Ok(Some(s)) => match s.trim().parse() {
            Ok(v) => v,
            Err(_) => return ReadOutcome::Err(format!("{}: bad memory.current", r.display)),
        },
        Ok(None) => return ReadOutcome::Gone,
        Err(e) => return ReadOutcome::Err(format!("{}: {e}", r.display)),
    };
    // memory.stat / memory.max are non-essential; default gracefully if absent.
    let inactive = read_cgroup(&format!("{}/memory.stat", r.cgroup_path))
        .ok()
        .flatten()
        .map(|s| parse_inactive_file(&s))
        .unwrap_or(0);
    let mem_limit = read_cgroup(&format!("{}/memory.max", r.cgroup_path))
        .ok()
        .flatten()
        .map(|s| parse_mem_max(&s))
        .unwrap_or(0);
    let now = Instant::now();
    let cpu_pct = match prev.get(&r.cgroup_path) {
        Some((prev_usage, prev_at)) => cgroup_cpu_pct(
            usage.saturating_sub(*prev_usage),
            now.duration_since(*prev_at).as_micros() as u64,
        ),
        None => 0.0, // first poll: no prior sample to delta against
    };
    prev.insert(r.cgroup_path.clone(), (usage, now));
    ReadOutcome::Ok(RunnerResource {
        name: r.display.clone(),
        cpu_pct,
        mem_bytes: mem_used(current, inactive),
        mem_limit,
        key: r.key.clone(),
        kind: SourceKind::Native,
    })
}

/// Turn per-runner errors into the poll result. Any genuine read error yields a
/// banner and an empty resource set, so the app keeps its last-known-good native
/// slice (mirroring the docker path's whole-collection preserve-on-error).
fn finalize(
    resources: Vec<RunnerResource>,
    errs: Vec<String>,
) -> (Vec<RunnerResource>, Option<String>) {
    if errs.is_empty() {
        (resources, None)
    } else {
        (Vec::new(), Some(format!("native: {}", errs.join("; "))))
    }
}

pub async fn run(runners: Vec<NativeRunner>, tx: mpsc::Sender<ResourceUpdate>) {
    // Wall-clock + usage_usec of the previous poll, keyed by cgroup path.
    let mut prev: HashMap<String, (u64, Instant)> = HashMap::new();
    loop {
        let mut resources = Vec::new();
        let mut errs = Vec::new();
        for r in &runners {
            match collect_one(r, &mut prev) {
                ReadOutcome::Ok(res) => resources.push(res),
                ReadOutcome::Gone => {
                    prev.remove(&r.cgroup_path); // stopped: forget its CPU baseline
                }
                ReadOutcome::Err(e) => errs.push(e),
            }
        }
        let (resources, error) = finalize(resources, errs);
        let _ = tx
            .send(ResourceUpdate {
                source: SourceKind::Native,
                resources,
                matched_seen: 0,
                unmatched_seen: 0,
                error,
            })
            .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A throwaway directory that mimics a runner's cgroup, populated with the
    /// given `(filename, contents)` files. Auto-removed on drop.
    struct FakeCgroup(String);

    impl FakeCgroup {
        fn new(files: &[(&str, &str)]) -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "pitwall-cg-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            for (name, contents) in files {
                std::fs::write(dir.join(name), contents).unwrap();
            }
            FakeCgroup(dir.to_string_lossy().into_owned())
        }

        fn runner(&self) -> NativeRunner {
            NativeRunner {
                display: "scoop-vanscout".into(),
                cgroup_path: self.0.clone(),
                scope: None,
                key: None,
            }
        }
    }

    impl Drop for FakeCgroup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn collect_one_reads_working_set_and_deltas_cpu() {
        let cg = FakeCgroup::new(&[
            ("cpu.stat", "usage_usec 1000000\nuser_usec 1\n"),
            ("memory.current", "119373824\n"),
            ("memory.stat", "anon 1\ninactive_file 15044608\n"),
            ("memory.max", "max\n"),
        ]);
        let r = cg.runner();
        let mut prev = HashMap::new();
        // First poll: CPU 0 (no baseline), working-set memory subtracts inactive_file.
        match collect_one(&r, &mut prev) {
            ReadOutcome::Ok(res) => {
                assert_eq!(res.cpu_pct, 0.0);
                assert_eq!(res.mem_bytes, 119373824 - 15044608);
                assert_eq!(res.mem_limit, 0); // uncapped
            }
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn collect_one_absent_cgroup_is_gone_not_error() {
        // A stopped unit: its cgroup dir/files no longer exist → Gone (drop silently).
        let r = NativeRunner {
            display: "scoop-vanscout".into(),
            cgroup_path: "/nonexistent/pitwall/cgroup".into(),
            scope: None,
            key: None,
        };
        let mut prev = HashMap::new();
        assert!(matches!(collect_one(&r, &mut prev), ReadOutcome::Gone));
    }

    #[test]
    fn collect_one_unreadable_file_is_error() {
        // cpu.stat present but not a regular file (a dir) → a real read error.
        let cg = FakeCgroup::new(&[("memory.current", "1\n")]);
        std::fs::create_dir_all(format!("{}/cpu.stat", cg.0)).unwrap();
        let mut prev = HashMap::new();
        match collect_one(&cg.runner(), &mut prev) {
            ReadOutcome::Err(e) => assert!(e.contains("scoop-vanscout")),
            _ => panic!("expected Err"),
        }
    }

    #[test]
    fn finalize_reports_and_preserves_on_error() {
        // No errors → fresh resources pass through, no banner.
        let res = vec![RunnerResource {
            name: "ltdovr".into(),
            cpu_pct: 0.0,
            mem_bytes: 1,
            mem_limit: 0,
            key: None,
            kind: SourceKind::Native,
        }];
        let (out, err) = finalize(res.clone(), vec![]);
        assert_eq!(out.len(), 1);
        assert!(err.is_none());
        // Any error → empty set (app keeps last-known-good) + a banner naming it.
        let (out, err) = finalize(res, vec!["scoop-vanscout: denied".into()]);
        assert!(out.is_empty());
        assert_eq!(err.as_deref(), Some("native: scoop-vanscout: denied"));
    }

    #[test]
    fn parse_units_keeps_only_actions_runner_services() {
        let out =
            "actions.runner.ltdovr.backontop.service loaded active running GitHub Actions Runner\n\
                   actions.runner.scoop-vanscout.backontop.service loaded active running x\n\
                   some-other.service loaded active running y\n";
        assert_eq!(
            parse_units_output(out),
            vec![
                "actions.runner.ltdovr.backontop.service",
                "actions.runner.scoop-vanscout.backontop.service"
            ]
        );
    }

    #[test]
    fn parse_show_extracts_controlgroup_and_workdir() {
        let s = "ControlGroup=/system.slice/actions.runner.ltdovr.backontop.service\n\
                 WorkingDirectory=/home/patrick/Work/flowagent-runner\n";
        let (cg, wd) = parse_show(s);
        assert_eq!(
            cg.as_deref(),
            Some("/system.slice/actions.runner.ltdovr.backontop.service")
        );
        assert_eq!(wd.as_deref(), Some("/home/patrick/Work/flowagent-runner"));
    }

    #[test]
    fn unit_display_takes_registration_segment() {
        assert_eq!(
            unit_display("actions.runner.scoop-kanban-api.backontop-kanban-api.service").as_deref(),
            Some("scoop-kanban-api")
        );
        assert_eq!(
            unit_display("actions.runner.ltdovr.backontop.service").as_deref(),
            Some("ltdovr")
        );
    }

    #[test]
    fn parse_scope_distinguishes_org_from_repo() {
        assert_eq!(
            parse_scope("https://github.com/ltdovr"),
            Some(Scope::Org("ltdovr".into()))
        );
        assert_eq!(
            parse_scope("https://github.com/scoop/kanban-api"),
            Some(Scope::Repo("scoop/kanban-api".into()))
        );
        assert_eq!(
            parse_scope("https://github.com/scoop/mensamax-ui/"),
            Some(Scope::Repo("scoop/mensamax-ui".into()))
        );
    }

    #[test]
    fn parse_dot_runner_tolerates_bom() {
        let json = "\u{feff}{\"agentName\":\"backontop-kanban-api\",\"gitHubUrl\":\"https://github.com/scoop/kanban-api\"}";
        let (agent, scope) = parse_dot_runner(json).unwrap();
        assert_eq!(agent, "backontop-kanban-api");
        assert_eq!(scope, Scope::Repo("scoop/kanban-api".into()));
    }

    #[test]
    fn cgroup_stat_parsers() {
        let cpu = "usage_usec 760229237\nuser_usec 354270695\nsystem_usec 405958541\n";
        assert_eq!(parse_usage_usec(cpu), Some(760229237));
        let mem = "anon 100\ninactive_file 15044608\nfile 200\n";
        assert_eq!(parse_inactive_file(mem), 15044608);
        assert_eq!(parse_inactive_file("anon 100\n"), 0); // key absent → 0
        assert_eq!(parse_mem_max("max\n"), 0);
        assert_eq!(parse_mem_max("8589934592\n"), 8589934592);
    }

    #[test]
    fn build_runner_with_dot_runner_has_matchable_key() {
        let dot = "{\"agentName\":\"backontop-vanscout\",\"gitHubUrl\":\"https://github.com/scoop/vanscout\"}";
        let r = build_runner(
            "actions.runner.scoop-vanscout.backontop.service",
            "/sys/fs/cgroup/system.slice/x".into(),
            Some(dot),
        )
        .unwrap();
        assert_eq!(r.display, "scoop-vanscout");
        assert_eq!(
            r.key,
            Some(RunnerKey {
                scope: "scoop/vanscout".into(),
                name: "backontop-vanscout".into()
            })
        );
        assert_eq!(r.scope, Some(Scope::Repo("scoop/vanscout".into())));
    }

    #[test]
    fn build_runner_without_readable_dot_runner_is_resource_only() {
        // Unreadable `.runner` (None) → still discovered, but no key/scope.
        let r = build_runner(
            "actions.runner.ltdovr.backontop.service",
            "/sys/fs/cgroup/system.slice/x".into(),
            None,
        )
        .unwrap();
        assert_eq!(r.display, "ltdovr");
        assert!(r.key.is_none());
        assert!(r.scope.is_none());
    }

    #[test]
    fn derive_scopes_dedups_repos_and_collects_orgs() {
        let runners = vec![
            build_runner(
                "actions.runner.scoop-vanscout.backontop.service",
                "/cg".into(),
                Some("{\"agentName\":\"backontop-vanscout\",\"gitHubUrl\":\"https://github.com/scoop/vanscout\"}"),
            )
            .unwrap(),
            build_runner(
                "actions.runner.ltdovr.backontop.service",
                "/cg".into(),
                Some("{\"agentName\":\"backontop\",\"gitHubUrl\":\"https://github.com/ltdovr\"}"),
            )
            .unwrap(),
            // Overlaps the pulse repo — must not be polled twice.
            build_runner(
                "actions.runner.dup.host.service",
                "/cg".into(),
                Some("{\"agentName\":\"dup\",\"gitHubUrl\":\"https://github.com/erwins-enkel/pulse\"}"),
            )
            .unwrap(),
            // Resource-only (no scope) contributes nothing.
            build_runner("actions.runner.blind.host.service", "/cg".into(), None).unwrap(),
        ];
        let (repos, orgs) = derive_scopes("erwins-enkel/pulse", &runners);
        assert_eq!(repos, vec!["erwins-enkel/pulse", "scoop/vanscout"]);
        assert_eq!(orgs, vec!["ltdovr"]);
    }

    #[test]
    fn discover_empty_when_systemctl_absent() {
        // systemctl returns None (binary missing / errors) → no rows, no panic.
        let runners = discover_inner(|_| None, |_| None);
        assert!(runners.is_empty());
    }

    #[test]
    fn discover_empty_when_no_units_listed() {
        let runners = discover_inner(|_| Some(String::new()), |_| None);
        assert!(runners.is_empty());
    }

    #[test]
    fn discover_assembles_runners_from_fakes() {
        let systemctl = |args: &[&str]| -> Option<String> {
            if args.first() == Some(&"list-units") {
                Some(
                    "actions.runner.scoop-vanscout.backontop.service loaded active running x\n"
                        .into(),
                )
            } else {
                // show ...
                Some(
                    "ControlGroup=/system.slice/actions.runner.scoop-vanscout.backontop.service\n\
                     WorkingDirectory=/work/vanscout-runner\n"
                        .into(),
                )
            }
        };
        let read_file = |p: &str| -> Option<String> {
            assert_eq!(p, "/work/vanscout-runner/.runner");
            Some("{\"agentName\":\"backontop-vanscout\",\"gitHubUrl\":\"https://github.com/scoop/vanscout\"}".into())
        };
        let runners = discover_inner(systemctl, read_file);
        assert_eq!(runners.len(), 1);
        assert_eq!(runners[0].display, "scoop-vanscout");
        assert_eq!(
            runners[0].cgroup_path,
            "/sys/fs/cgroup/system.slice/actions.runner.scoop-vanscout.backontop.service"
        );
        assert_eq!(runners[0].key.as_ref().unwrap().scope, "scoop/vanscout");
    }
}
