use anyhow::Context;
use serde::Deserialize;
use std::path::PathBuf;

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct Config {
    pub socket_path: String,
    pub repo: String,
    pub prefix: String,
    pub slice_cap_bytes: u64,
}

/// Optional settings parsed from the TOML config file. Every field is optional:
/// a missing field falls through to the matching env var (which overrides the
/// file) or the built-in default. Unknown keys are rejected so typos surface
/// loudly instead of being silently ignored.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    socket: Option<String>,
    repo: Option<String>,
    prefix: Option<String>,
    slice_cap_gib: Option<u64>,
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

    let repo = env_nonempty(get, "PITWALL_REPO")
        .or(file.repo)
        .unwrap_or_else(|| "owner/repo".into());

    let prefix = env_nonempty(get, "PITWALL_PREFIX")
        .or(file.prefix)
        .unwrap_or_else(|| "ci-runner-".into());

    let cap_gib = env_nonempty(get, "PITWALL_SLICE_CAP_GIB")
        .and_then(|s| s.parse::<u64>().ok())
        .or(file.slice_cap_gib)
        .unwrap_or(24);

    Config {
        socket_path,
        repo,
        prefix,
        slice_cap_bytes: cap_gib.saturating_mul(GIB),
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

    // ---- resolve() -------------------------------------------------------

    #[test]
    fn defaults_when_env_and_file_empty() {
        let c = resolve(FileConfig::default(), &env(&[]));
        assert_eq!(c.repo, "owner/repo");
        assert_eq!(c.prefix, "ci-runner-");
        assert_eq!(c.slice_cap_bytes, 24 * GIB);
        assert_eq!(c.socket_path, default_socket_path());
    }

    #[test]
    fn file_values_used_when_env_unset() {
        let file = FileConfig {
            socket: Some("/file.sock".into()),
            repo: Some("o/r".into()),
            prefix: Some("px-".into()),
            slice_cap_gib: Some(8),
        };
        let c = resolve(file, &env(&[]));
        assert_eq!(c.socket_path, "/file.sock");
        assert_eq!(c.repo, "o/r");
        assert_eq!(c.prefix, "px-");
        assert_eq!(c.slice_cap_bytes, 8 * GIB);
    }

    #[test]
    fn env_overrides_file() {
        let file = FileConfig {
            socket: Some("/file.sock".into()),
            repo: Some("file/repo".into()),
            prefix: Some("file-".into()),
            slice_cap_gib: Some(8),
        };
        let c = resolve(
            file,
            &env(&[
                ("PITWALL_SOCKET", "/env.sock"),
                ("PITWALL_REPO", "env/repo"),
                ("PITWALL_PREFIX", "env-"),
                ("PITWALL_SLICE_CAP_GIB", "16"),
            ]),
        );
        assert_eq!(c.socket_path, "/env.sock");
        assert_eq!(c.repo, "env/repo");
        assert_eq!(c.prefix, "env-");
        assert_eq!(c.slice_cap_bytes, 16 * GIB);
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
            repo: Some("file/repo".into()),
            ..Default::default()
        };
        let c = resolve(file, &env(&[("PITWALL_SOCKET", ""), ("PITWALL_REPO", "")]));
        assert_eq!(c.socket_path, "/file.sock");
        assert_eq!(c.repo, "file/repo");
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
        assert_eq!(cfg.repo.as_deref(), Some("o/r"));
        assert_eq!(cfg.slice_cap_gib, Some(4));
    }

    #[test]
    fn load_unknown_key_is_error() {
        let p = temp_path("unknown-key.toml");
        std::fs::write(&p, "bogus = true\n").unwrap();
        let res = load_file(ConfigPath::Explicit(p.clone()));
        std::fs::remove_file(&p).unwrap();
        assert!(res.is_err());
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
