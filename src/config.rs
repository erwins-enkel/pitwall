use crate::theme::Flavor;
use anyhow::Context;
use serde::Deserialize;
use std::path::PathBuf;

const GIB: u64 = 1024 * 1024 * 1024;

/// One runner container-name prefix and the repo its jobs live under. `repo`
/// is `None` for an unmapped prefix — the docker collector then tags matching
/// runners with `key: None` (they render idle; no job lookup), so two unmapped
/// fleets can never collide on a shared scope. The first rule additionally
/// inherits the first configured repo at resolve time for backward compat.
#[derive(Clone, Debug, PartialEq)]
pub struct PrefixRule {
    pub prefix: String,
    pub repo: Option<String>,
}

#[derive(Clone)]
pub struct Config {
    pub socket_path: String,
    pub prefixes: Vec<PrefixRule>,
    pub slice_cap_bytes: u64,
    pub flavor: Flavor,
    pub warn_ratio: f64,
    pub crit_ratio: f64,
    /// Repos to poll for in-progress job detail. Defaults to `configured_repos`; `app`
    /// augments this with native runners' repo-scopes (deduplicated) at startup.
    pub repos: Vec<String>,
    /// Orgs to poll for runner busy status (native org-scoped runners). Empty by
    /// default; populated from native discovery at startup.
    pub orgs: Vec<String>,
    /// User-configured repos (from `PITWALL_REPO` / the TOML `repo` key). Empty
    /// when unset. `app` seeds `derive_scopes` from this; the derived poll list
    /// lands in `repos`.
    pub configured_repos: Vec<String>,
}

const DEFAULT_WARN_PCT: u64 = 85;
const DEFAULT_CRIT_PCT: u64 = 90;

/// Parse a memory-threshold env value as an integer percent, clamped to
/// `0..=100`. A missing or non-numeric value falls back to `default`.
fn parse_pct(raw: Option<&str>, default: u64) -> u64 {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(default)
        .min(100)
}

/// Resolve the warn/critical memory thresholds (as fractions `0.0..=1.0`) from
/// their raw env values. Pure — takes the raw strings as arguments so it can be
/// unit-tested without touching process-wide env. `warn` is pinned to `crit`
/// when it would otherwise exceed it, so the two tiers can never invert.
fn resolve_thresholds(warn_raw: Option<&str>, crit_raw: Option<&str>) -> (f64, f64) {
    let crit = parse_pct(crit_raw, DEFAULT_CRIT_PCT);
    let warn = parse_pct(warn_raw, DEFAULT_WARN_PCT).min(crit);
    (warn as f64 / 100.0, crit as f64 / 100.0)
}

/// The TOML `repo` key accepts either a single repo string or an array of them
/// (each entry may itself be comma-separated). Untagged so both forms parse
/// transparently.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RepoField {
    One(String),
    Many(Vec<String>),
}

/// One table entry in an array-form `prefix` key: the container-name prefix and
/// an optional repo its jobs live under. `match` is required (a missing one is a
/// hard parse error); unknown keys are rejected.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrefixEntry {
    #[serde(rename = "match")]
    prefix: String,
    repo: Option<String>,
}

/// The TOML `prefix` key accepts either a single prefix string (unmapped) or an
/// array of `{ match, repo }` tables. Untagged so both forms parse transparently.
/// Array-of-strings and mixed forms are intentionally unsupported — a second
/// unmapped prefix is written as `{ match = "..." }` with `repo` omitted.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PrefixField {
    One(String),
    Many(Vec<PrefixEntry>),
}

/// Optional settings parsed from the TOML config file. Every field is optional:
/// a missing field falls through to the matching env var (which overrides the
/// file) or the built-in default. Unknown keys are rejected so typos surface
/// loudly instead of being silently ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    socket: Option<String>,
    repo: Option<RepoField>,
    prefix: Option<PrefixField>,
    slice_cap_gib: Option<u64>,
    theme: Option<String>,
}

/// Where the config file lives, and whether the path was chosen explicitly.
#[derive(Debug, PartialEq)]
enum ConfigPath {
    /// `PITWALL_CONFIG` pointed here — the file MUST exist (a missing explicit
    /// path is an error, not a silent fall-back).
    Explicit(PathBuf),
    /// Default XDG/HOME location — its absence is fine (empty config).
    Default(PathBuf),
    /// No usable `PITWALL_CONFIG`, `XDG_CONFIG_HOME`, or `HOME` — no file.
    None,
}

/// Extract a unix socket path from a `DOCKER_HOST` value. Returns `None` for
/// non-unix schemes (`tcp://`, `ssh://`, `http(s)://`, …) which this
/// rootless-docker tool cannot use over a unix socket — the caller then falls
/// back to the default `/run/user/$UID/docker.sock`.
fn unix_socket_from_docker_host(h: &str) -> Option<String> {
    if let Some(path) = h.strip_prefix("unix://") {
        Some(path.to_string())
    } else if h.starts_with('/') {
        // A bare absolute path is a unix socket path.
        Some(h.to_string())
    } else {
        None
    }
}

/// Read an env var, treating both unset and empty-string as "not set" so that
/// e.g. `PITWALL_SOCKET=` never masks a config-file value with the default.
fn env_nonempty(get: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    get(key).filter(|v| !v.is_empty())
}

fn default_socket_path() -> String {
    let uid = unsafe { libc::getuid() };
    format!("/run/user/{uid}/docker.sock")
}

/// Resolve where the config file should be read from. `PITWALL_CONFIG` (when
/// non-empty) wins; otherwise `$XDG_CONFIG_HOME/pitwall/config.toml`, falling
/// back to `$HOME/.config/pitwall/config.toml`. An empty `PITWALL_CONFIG` is
/// treated as unset, consistent with every other env var.
fn resolve_config_path(get: &dyn Fn(&str) -> Option<String>) -> ConfigPath {
    if let Some(p) = env_nonempty(get, "PITWALL_CONFIG") {
        return ConfigPath::Explicit(PathBuf::from(p));
    }
    if let Some(xdg) = env_nonempty(get, "XDG_CONFIG_HOME") {
        return ConfigPath::Default(PathBuf::from(xdg).join("pitwall").join("config.toml"));
    }
    if let Some(home) = env_nonempty(get, "HOME") {
        return ConfigPath::Default(
            PathBuf::from(home)
                .join(".config")
                .join("pitwall")
                .join("config.toml"),
        );
    }
    ConfigPath::None
}

/// Read and parse the config file. A default-path file that is absent yields
/// `Ok(None)`; an explicitly-requested file that is missing/unreadable, or any
/// file that fails to parse (incl. unknown keys), is an error.
fn load_file(cp: ConfigPath) -> anyhow::Result<Option<FileConfig>> {
    let (path, required) = match cp {
        ConfigPath::Explicit(p) => (p, true),
        ConfigPath::Default(p) => (p, false),
        ConfigPath::None => return Ok(None),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading config file {}", path.display())),
    };
    let cfg =
        toml::from_str(&text).with_context(|| format!("parsing config file {}", path.display()))?;
    Ok(Some(cfg))
}

/// Split a repo spec on commas, trimming each entry and dropping empties.
fn split_repos(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(String::from)
        .collect()
}

/// Order-preserving dedup — keeps the first occurrence of each repo.
fn dedup_preserve(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(v.len());
    for s in v {
        if !out.iter().any(|e| e == &s) {
            out.push(s);
        }
    }
    out
}

/// Flatten a TOML `repo` field (string or array; entries may be comma-separated)
/// into a repo list.
fn repo_field_to_vec(f: Option<RepoField>) -> Vec<String> {
    match f {
        Some(RepoField::One(s)) => split_repos(&s),
        Some(RepoField::Many(v)) => v.iter().flat_map(|s| split_repos(s)).collect(),
        None => Vec::new(),
    }
}

/// Parse one `PITWALL_PREFIX` entry: `"prefix"` or `"prefix=owner/repo"`. The
/// prefix is trimmed; a present, non-empty repo half maps the prefix, else it's
/// unmapped. An empty prefix yields `None` (dropped by the caller).
fn parse_env_prefix_entry(raw: &str) -> Option<PrefixRule> {
    let (prefix, repo) = match raw.split_once('=') {
        Some((p, r)) => (p.trim(), Some(r.trim())),
        None => (raw.trim(), None),
    };
    if prefix.is_empty() {
        return None;
    }
    Some(PrefixRule {
        prefix: prefix.to_string(),
        repo: repo.filter(|r| !r.is_empty()).map(String::from),
    })
}

/// Flatten the `prefix` source into rules. Env (comma-separated, each entry with
/// an optional `=owner/repo` suffix) wins over the file's string-or-table form.
/// An empty result falls back to the single built-in default prefix.
fn resolve_prefixes(env_val: Option<String>, file: Option<PrefixField>) -> Vec<PrefixRule> {
    let mut rules: Vec<PrefixRule> = match env_val {
        Some(v) => v.split(',').filter_map(parse_env_prefix_entry).collect(),
        None => match file {
            Some(PrefixField::One(s)) if !s.trim().is_empty() => vec![PrefixRule {
                prefix: s.trim().to_string(),
                repo: None,
            }],
            Some(PrefixField::One(_)) | None => Vec::new(),
            Some(PrefixField::Many(v)) => v
                .into_iter()
                .filter(|e| !e.prefix.trim().is_empty())
                .map(|e| PrefixRule {
                    prefix: e.prefix.trim().to_string(),
                    repo: e
                        .repo
                        .filter(|r| !r.trim().is_empty())
                        .map(|r| r.trim().to_string()),
                })
                .collect(),
        },
    };
    if rules.is_empty() {
        rules.push(PrefixRule {
            prefix: "ci-runner-".into(),
            repo: None,
        });
    }
    rules
}

/// Preserve today's single-prefix behavior: the first rule, when unmapped,
/// inherits the first configured repo so its docker runners still resolve job
/// detail. Every *other* unmapped rule stays `None` (idle) so two unmapped
/// fleets can't share a scope. No-op when the first rule is already mapped or no
/// repo is configured.
fn fold_implicit_first_repo(rules: &mut [PrefixRule], configured_repos: &[String]) {
    if let Some(first) = rules.first_mut() {
        if first.repo.is_none() {
            if let Some(repo) = configured_repos.first() {
                first.repo = Some(repo.clone());
            }
        }
    }
}

/// Repos to poll for job detail: the user's configured repos plus every rule's
/// mapped repo, order-preserving and deduplicated. Feeds `derive_scopes`;
/// `configured_repos` itself is left untouched so the collector never invents a
/// scope from a mapping.
pub fn prefix_poll_seed(configured_repos: &[String], prefixes: &[PrefixRule]) -> Vec<String> {
    let mut seed: Vec<String> = configured_repos.to_vec();
    seed.extend(prefixes.iter().filter_map(|r| r.repo.clone()));
    dedup_preserve(seed)
}

/// Layer the file config under the environment: for each key an env var (when
/// set to a usable value) wins, else the file value, else the built-in default.
/// `socket` additionally consults `DOCKER_HOST` between the file value and the
/// UID default. An unparseable `PITWALL_SLICE_CAP_GIB` counts as unset and
/// falls through rather than reverting to the default.
fn resolve(file: FileConfig, get: &dyn Fn(&str) -> Option<String>) -> Config {
    let socket_path = env_nonempty(get, "PITWALL_SOCKET")
        .or(file.socket)
        .or_else(|| env_nonempty(get, "DOCKER_HOST").and_then(|h| unix_socket_from_docker_host(&h)))
        .unwrap_or_else(default_socket_path);

    // Env (comma-separated) wins over the file's string-or-array; empty ⇒ unset.
    let configured_repos = match env_nonempty(get, "PITWALL_REPO") {
        Some(env_val) => dedup_preserve(split_repos(&env_val)),
        None => dedup_preserve(repo_field_to_vec(file.repo)),
    };

    let mut prefixes = resolve_prefixes(env_nonempty(get, "PITWALL_PREFIX"), file.prefix);
    fold_implicit_first_repo(&mut prefixes, &configured_repos);

    let cap_gib = env_nonempty(get, "PITWALL_SLICE_CAP_GIB")
        .and_then(|s| s.parse::<u64>().ok())
        .or(file.slice_cap_gib)
        .unwrap_or(24);

    // Theme parsing is infallible: env wins, then file, then default; any
    // unrecognized value maps to Mocha inside `parse_lenient`.
    let flavor = Flavor::parse_lenient(
        env_nonempty(get, "PITWALL_THEME")
            .or(file.theme)
            .unwrap_or_default()
            .as_str(),
    );

    let (warn_ratio, crit_ratio) = resolve_thresholds(
        env_nonempty(get, "PITWALL_MEM_WARN_PCT").as_deref(),
        env_nonempty(get, "PITWALL_MEM_CRIT_PCT").as_deref(),
    );

    Config {
        repos: configured_repos.clone(),
        orgs: vec![],
        socket_path,
        configured_repos,
        prefixes,
        slice_cap_bytes: cap_gib.saturating_mul(GIB),
        flavor,
        warn_ratio,
        crit_ratio,
    }
}

impl Config {
    /// Load config from the optional TOML file layered under the process
    /// environment. Fails only when a config file is present but invalid (or an
    /// explicit `PITWALL_CONFIG` path is unreadable).
    pub fn load() -> anyhow::Result<Config> {
        let get = |k: &str| std::env::var(k).ok();
        let file = load_file(resolve_config_path(&get))?.unwrap_or_default();
        Ok(resolve(file, &get))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake env accessor from `(key, value)` pairs. Keys not listed are
    /// unset (`None`); a listed empty value models `KEY=` in the environment.
    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("pitwall-cfgtest-{}-{name}", std::process::id()))
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // ---- resolve_thresholds() -------------------------------------------

    #[test]
    fn thresholds_default_when_unset() {
        let (w, c) = resolve_thresholds(None, None);
        assert!(approx(w, 0.85), "warn {w}");
        assert!(approx(c, 0.90), "crit {c}");
    }

    #[test]
    fn thresholds_clamp_over_100() {
        let (w, c) = resolve_thresholds(Some("150"), Some("200"));
        assert!(approx(w, 1.0) && approx(c, 1.0));
    }

    #[test]
    fn thresholds_non_numeric_falls_back_to_default() {
        let (w, c) = resolve_thresholds(Some("abc"), Some("xyz"));
        assert!(approx(w, 0.85) && approx(c, 0.90));
    }

    #[test]
    fn thresholds_warn_pinned_to_crit_when_inverted() {
        // warn > crit must not invert: warn is pinned down to crit.
        let (w, c) = resolve_thresholds(Some("95"), Some("80"));
        assert!(approx(w, 0.80) && approx(c, 0.80));
    }

    #[test]
    fn thresholds_degenerate_crit_zero_pins_warn_zero() {
        let (w, c) = resolve_thresholds(Some("50"), Some("0"));
        assert!(approx(w, 0.0) && approx(c, 0.0));
    }

    // ---- split_repos / dedup_preserve -----------------------------------

    #[test]
    fn split_repos_trims_and_drops_empties() {
        assert_eq!(split_repos("a/b, c/d"), vec!["a/b", "c/d"]);
        assert_eq!(split_repos("  x/y  "), vec!["x/y"]);
        assert_eq!(split_repos("a/b,,c/d,"), vec!["a/b", "c/d"]);
        assert!(split_repos("").is_empty());
        assert!(split_repos("  ,  ").is_empty());
    }

    #[test]
    fn dedup_preserve_keeps_first_occurrence_order() {
        assert_eq!(
            dedup_preserve(vec!["a/b".into(), "c/d".into(), "a/b".into(), "e/f".into()]),
            vec!["a/b", "c/d", "e/f"]
        );
    }

    // ---- resolve() multi-repo -------------------------------------------

    #[test]
    fn resolve_comma_env_yields_multiple_repos() {
        let c = resolve(FileConfig::default(), &env(&[("PITWALL_REPO", "a/b, c/d")]));
        assert_eq!(c.configured_repos, vec!["a/b", "c/d"]);
    }

    #[test]
    fn resolve_toml_array_yields_multiple_repos() {
        let file = FileConfig {
            repo: Some(RepoField::Many(vec!["a/b".into(), "c/d".into()])),
            ..Default::default()
        };
        let c = resolve(file, &env(&[]));
        assert_eq!(c.configured_repos, vec!["a/b", "c/d"]);
    }

    #[test]
    fn resolve_toml_string_with_comma_splits() {
        let file = FileConfig {
            repo: Some(RepoField::One("a/b,c/d".into())),
            ..Default::default()
        };
        let c = resolve(file, &env(&[]));
        assert_eq!(c.configured_repos, vec!["a/b", "c/d"]);
    }

    #[test]
    fn resolve_dedups_repeated_repos() {
        let c = resolve(
            FileConfig::default(),
            &env(&[("PITWALL_REPO", "a/b,a/b,c/d")]),
        );
        assert_eq!(c.configured_repos, vec!["a/b", "c/d"]);
    }

    // ---- prefixes --------------------------------------------------------

    fn rule(prefix: &str, repo: Option<&str>) -> PrefixRule {
        PrefixRule {
            prefix: prefix.into(),
            repo: repo.map(String::from),
        }
    }

    #[test]
    fn parse_env_prefix_entry_maps_and_trims() {
        assert_eq!(
            parse_env_prefix_entry("  pulse- = a/b "),
            Some(rule("pulse-", Some("a/b")))
        );
        assert_eq!(parse_env_prefix_entry("pulse-"), Some(rule("pulse-", None)));
        // Empty repo half is treated as unmapped.
        assert_eq!(
            parse_env_prefix_entry("pulse-="),
            Some(rule("pulse-", None))
        );
        // Empty prefix is dropped.
        assert_eq!(parse_env_prefix_entry("  "), None);
        assert_eq!(parse_env_prefix_entry("=a/b"), None);
    }

    #[test]
    fn resolve_env_prefix_multi_with_mapping() {
        let c = resolve(
            FileConfig::default(),
            &env(&[(
                "PITWALL_PREFIX",
                "pulse-ci-runner-=erwins-enkel/pulse,flowagent-ci-runner-=ltdovr/flowagent",
            )]),
        );
        assert_eq!(
            c.prefixes,
            vec![
                rule("pulse-ci-runner-", Some("erwins-enkel/pulse")),
                rule("flowagent-ci-runner-", Some("ltdovr/flowagent")),
            ]
        );
    }

    #[test]
    fn resolve_toml_prefix_table_form() {
        let file = FileConfig {
            prefix: Some(PrefixField::Many(vec![
                PrefixEntry {
                    prefix: "pulse-ci-runner-".into(),
                    repo: Some("erwins-enkel/pulse".into()),
                },
                PrefixEntry {
                    prefix: "flowagent-ci-runner-".into(),
                    repo: Some("ltdovr/flowagent".into()),
                },
            ])),
            ..Default::default()
        };
        let c = resolve(file, &env(&[]));
        assert_eq!(
            c.prefixes,
            vec![
                rule("pulse-ci-runner-", Some("erwins-enkel/pulse")),
                rule("flowagent-ci-runner-", Some("ltdovr/flowagent")),
            ]
        );
    }

    #[test]
    fn fold_implicit_first_repo_only_first_unmapped_rule() {
        // Two unmapped prefixes + one configured repo: ONLY the first inherits
        // it; the second stays None so the two fleets can't share a scope.
        let mut rules = vec![
            rule("pulse-ci-runner-", None),
            rule("flowagent-ci-runner-", None),
        ];
        fold_implicit_first_repo(&mut rules, &["erwins-enkel/pulse".to_string()]);
        assert_eq!(
            rules,
            vec![
                rule("pulse-ci-runner-", Some("erwins-enkel/pulse")),
                rule("flowagent-ci-runner-", None),
            ]
        );
    }

    #[test]
    fn fold_implicit_first_repo_noop_when_first_mapped_or_no_repo() {
        // Already-mapped first rule is untouched.
        let mut mapped = vec![rule("pulse-", Some("a/b"))];
        fold_implicit_first_repo(&mut mapped, &["c/d".to_string()]);
        assert_eq!(mapped, vec![rule("pulse-", Some("a/b"))]);
        // No configured repo → stays unmapped.
        let mut unmapped = vec![rule("pulse-", None)];
        fold_implicit_first_repo(&mut unmapped, &[]);
        assert_eq!(unmapped, vec![rule("pulse-", None)]);
    }

    #[test]
    fn resolve_two_unmapped_prefixes_folds_first_only() {
        let file = FileConfig {
            repo: Some(RepoField::One("erwins-enkel/pulse".into())),
            prefix: Some(PrefixField::Many(vec![
                PrefixEntry {
                    prefix: "pulse-ci-runner-".into(),
                    repo: None,
                },
                PrefixEntry {
                    prefix: "flowagent-ci-runner-".into(),
                    repo: None,
                },
            ])),
            ..Default::default()
        };
        let c = resolve(file, &env(&[]));
        assert_eq!(
            c.prefixes,
            vec![
                rule("pulse-ci-runner-", Some("erwins-enkel/pulse")),
                rule("flowagent-ci-runner-", None),
            ]
        );
    }

    #[test]
    fn prefix_poll_seed_merges_mapped_repos_deduped() {
        let configured = vec!["erwins-enkel/pulse".to_string()];
        let prefixes = vec![
            rule("pulse-ci-runner-", Some("erwins-enkel/pulse")), // dup of configured
            rule("flowagent-ci-runner-", Some("ltdovr/flowagent")),
            rule("misc-", None),
        ];
        assert_eq!(
            prefix_poll_seed(&configured, &prefixes),
            vec!["erwins-enkel/pulse", "ltdovr/flowagent"]
        );
    }

    // ---- resolve() -------------------------------------------------------

    #[test]
    fn defaults_when_env_and_file_empty() {
        let c = resolve(FileConfig::default(), &env(&[]));
        assert!(c.configured_repos.is_empty());
        // No repo configured → default prefix stays unmapped.
        assert_eq!(
            c.prefixes,
            vec![PrefixRule {
                prefix: "ci-runner-".into(),
                repo: None
            }]
        );
        assert_eq!(c.slice_cap_bytes, 24 * GIB);
        assert_eq!(c.socket_path, default_socket_path());
        assert_eq!(c.flavor, Flavor::Mocha);
    }

    #[test]
    fn file_values_used_when_env_unset() {
        let file = FileConfig {
            socket: Some("/file.sock".into()),
            repo: Some(RepoField::One("o/r".into())),
            prefix: Some(PrefixField::One("px-".into())),
            slice_cap_gib: Some(8),
            theme: Some("latte".into()),
        };
        let c = resolve(file, &env(&[]));
        assert_eq!(c.socket_path, "/file.sock");
        assert_eq!(c.configured_repos, vec!["o/r"]);
        // First (only) prefix inherits the configured repo.
        assert_eq!(
            c.prefixes,
            vec![PrefixRule {
                prefix: "px-".into(),
                repo: Some("o/r".into())
            }]
        );
        assert_eq!(c.slice_cap_bytes, 8 * GIB);
        // File `theme` is honored when the env var is unset.
        assert_eq!(c.flavor, Flavor::Latte);
    }

    #[test]
    fn env_overrides_file() {
        let file = FileConfig {
            socket: Some("/file.sock".into()),
            repo: Some(RepoField::One("file/repo".into())),
            prefix: Some(PrefixField::One("file-".into())),
            slice_cap_gib: Some(8),
            theme: Some("latte".into()),
        };
        let c = resolve(
            file,
            &env(&[
                ("PITWALL_SOCKET", "/env.sock"),
                ("PITWALL_REPO", "env/repo"),
                ("PITWALL_PREFIX", "env-"),
                ("PITWALL_SLICE_CAP_GIB", "16"),
                ("PITWALL_THEME", "frappe"),
            ]),
        );
        assert_eq!(c.socket_path, "/env.sock");
        assert_eq!(c.configured_repos, vec!["env/repo"]);
        // Env prefix wins over the file's; first prefix inherits the env repo.
        assert_eq!(
            c.prefixes,
            vec![PrefixRule {
                prefix: "env-".into(),
                repo: Some("env/repo".into())
            }]
        );
        assert_eq!(c.slice_cap_bytes, 16 * GIB);
        assert_eq!(c.flavor, Flavor::Frappe);
    }

    #[test]
    fn garbage_cap_env_falls_through_to_file() {
        let file = FileConfig {
            slice_cap_gib: Some(12),
            ..Default::default()
        };
        let c = resolve(file, &env(&[("PITWALL_SLICE_CAP_GIB", "notanumber")]));
        assert_eq!(c.slice_cap_bytes, 12 * GIB);
    }

    #[test]
    fn empty_env_treated_as_unset() {
        let file = FileConfig {
            socket: Some("/file.sock".into()),
            repo: Some(RepoField::One("file/repo".into())),
            ..Default::default()
        };
        let c = resolve(file, &env(&[("PITWALL_SOCKET", ""), ("PITWALL_REPO", "")]));
        assert_eq!(c.socket_path, "/file.sock");
        assert_eq!(c.configured_repos, vec!["file/repo"]);
    }

    #[test]
    fn socket_file_beats_docker_host() {
        let file = FileConfig {
            socket: Some("/file.sock".into()),
            ..Default::default()
        };
        let c = resolve(file, &env(&[("DOCKER_HOST", "unix:///dh.sock")]));
        assert_eq!(c.socket_path, "/file.sock");
    }

    #[test]
    fn socket_pitwall_env_beats_file() {
        let file = FileConfig {
            socket: Some("/file.sock".into()),
            ..Default::default()
        };
        let c = resolve(file, &env(&[("PITWALL_SOCKET", "/env.sock")]));
        assert_eq!(c.socket_path, "/env.sock");
    }

    #[test]
    fn socket_docker_host_used_when_no_pitwall_or_file() {
        let c = resolve(
            FileConfig::default(),
            &env(&[("DOCKER_HOST", "unix:///dh.sock")]),
        );
        assert_eq!(c.socket_path, "/dh.sock");
    }

    #[test]
    fn cap_gib_saturates_instead_of_overflowing() {
        let file = FileConfig {
            slice_cap_gib: Some(u64::MAX),
            ..Default::default()
        };
        let c = resolve(file, &env(&[]));
        assert_eq!(c.slice_cap_bytes, u64::MAX);
    }

    // ---- resolve_config_path() ------------------------------------------

    #[test]
    fn path_explicit_from_pitwall_config() {
        let cp = resolve_config_path(&env(&[("PITWALL_CONFIG", "/x/y.toml")]));
        assert_eq!(cp, ConfigPath::Explicit(PathBuf::from("/x/y.toml")));
    }

    #[test]
    fn path_empty_pitwall_config_falls_through() {
        let cp = resolve_config_path(&env(&[("PITWALL_CONFIG", ""), ("XDG_CONFIG_HOME", "/cfg")]));
        assert_eq!(
            cp,
            ConfigPath::Default(PathBuf::from("/cfg/pitwall/config.toml"))
        );
    }

    #[test]
    fn path_xdg_preferred_over_home() {
        let cp = resolve_config_path(&env(&[("XDG_CONFIG_HOME", "/cfg"), ("HOME", "/home/u")]));
        assert_eq!(
            cp,
            ConfigPath::Default(PathBuf::from("/cfg/pitwall/config.toml"))
        );
    }

    #[test]
    fn path_home_fallback() {
        let cp = resolve_config_path(&env(&[("HOME", "/home/u")]));
        assert_eq!(
            cp,
            ConfigPath::Default(PathBuf::from("/home/u/.config/pitwall/config.toml"))
        );
    }

    #[test]
    fn path_none_when_nothing_set() {
        assert_eq!(resolve_config_path(&env(&[])), ConfigPath::None);
    }

    // ---- load_file() -----------------------------------------------------

    #[test]
    fn load_explicit_missing_is_error() {
        let p = temp_path("explicit-missing.toml");
        let _ = std::fs::remove_file(&p);
        assert!(load_file(ConfigPath::Explicit(p)).is_err());
    }

    #[test]
    fn load_default_absent_is_none() {
        let p = temp_path("default-absent.toml");
        let _ = std::fs::remove_file(&p);
        assert!(load_file(ConfigPath::Default(p)).unwrap().is_none());
    }

    #[test]
    fn load_none_is_none() {
        assert!(load_file(ConfigPath::None).unwrap().is_none());
    }

    #[test]
    fn load_valid_toml_parses() {
        let p = temp_path("valid.toml");
        std::fs::write(&p, "repo = \"o/r\"\nslice_cap_gib = 4\n").unwrap();
        let cfg = load_file(ConfigPath::Explicit(p.clone())).unwrap().unwrap();
        std::fs::remove_file(&p).unwrap();
        assert!(matches!(cfg.repo, Some(RepoField::One(ref s)) if s == "o/r"));
        assert_eq!(cfg.slice_cap_gib, Some(4));
    }

    #[test]
    fn load_toml_repo_array_parses() {
        let p = temp_path("repo-array.toml");
        std::fs::write(&p, "repo = [\"o/r\", \"a/b\"]\n").unwrap();
        let cfg = load_file(ConfigPath::Explicit(p.clone())).unwrap().unwrap();
        std::fs::remove_file(&p).unwrap();
        assert!(matches!(cfg.repo, Some(RepoField::Many(ref v)) if v == &["o/r", "a/b"]));
    }

    #[test]
    fn load_unknown_key_is_error() {
        let p = temp_path("unknown-key.toml");
        std::fs::write(&p, "bogus = true\n").unwrap();
        let res = load_file(ConfigPath::Explicit(p.clone()));
        std::fs::remove_file(&p).unwrap();
        assert!(res.is_err());
    }

    #[test]
    fn load_toml_prefix_table_form_parses() {
        let p = temp_path("prefix-table.toml");
        std::fs::write(
            &p,
            "prefix = [{ match = \"pulse-ci-runner-\", repo = \"o/r\" }, { match = \"flowagent-ci-runner-\" }]\n",
        )
        .unwrap();
        let cfg = load_file(ConfigPath::Explicit(p.clone())).unwrap().unwrap();
        std::fs::remove_file(&p).unwrap();
        match cfg.prefix {
            Some(PrefixField::Many(v)) => {
                assert_eq!(v.len(), 2);
                assert_eq!(v[0].prefix, "pulse-ci-runner-");
                assert_eq!(v[0].repo.as_deref(), Some("o/r"));
                assert_eq!(v[1].prefix, "flowagent-ci-runner-");
                assert_eq!(v[1].repo, None);
            }
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn load_toml_prefix_table_missing_match_is_error() {
        let p = temp_path("prefix-no-match.toml");
        std::fs::write(&p, "prefix = [{ repo = \"o/r\" }]\n").unwrap();
        let res = load_file(ConfigPath::Explicit(p.clone()));
        std::fs::remove_file(&p).unwrap();
        assert!(res.is_err());
    }

    /// The checked-in `config.example.toml` must stay loadable: valid TOML, no
    /// unknown/renamed keys (else `deny_unknown_fields` rejects it), and its one
    /// active key `repo` deserializing as documented. Guards the example against
    /// silent drift when a `FileConfig` key is renamed.
    #[test]
    fn shipped_example_config_loads() {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.example.toml");
        let cfg = load_file(ConfigPath::Explicit(p))
            .expect("config.example.toml must parse")
            .expect("config.example.toml must exist");
        assert!(matches!(cfg.repo, Some(RepoField::One(ref s)) if s == "your-org/your-repo"));
    }

    // ---- unix_socket_from_docker_host() ---------------------------------

    #[test]
    fn docker_host_unix_paths_parsed_non_unix_ignored() {
        assert_eq!(
            unix_socket_from_docker_host("unix:///run/user/1000/docker.sock").as_deref(),
            Some("/run/user/1000/docker.sock")
        );
        assert_eq!(
            unix_socket_from_docker_host("/var/run/docker.sock").as_deref(),
            Some("/var/run/docker.sock")
        );
        // Non-unix schemes are unusable over a unix socket → None (caller uses default).
        assert_eq!(unix_socket_from_docker_host("tcp://host:2375"), None);
        assert_eq!(unix_socket_from_docker_host("ssh://user@host"), None);
    }
}
