#[derive(Clone)]
pub struct Config {
    pub socket_path: String,
    pub repo: String,
    pub prefix: String,
    pub slice_cap_bytes: u64,
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

impl Config {
    pub fn from_env() -> Config {
        let socket_path = std::env::var("PITWALL_SOCKET").ok().unwrap_or_else(|| {
            std::env::var("DOCKER_HOST")
                .ok()
                .and_then(|h| unix_socket_from_docker_host(&h))
                .unwrap_or_else(|| {
                    let uid = unsafe { libc::getuid() };
                    format!("/run/user/{uid}/docker.sock")
                })
        });
        let repo = std::env::var("PITWALL_REPO").unwrap_or_else(|_| "erwins-enkel/pulse".into());
        let prefix = std::env::var("PITWALL_PREFIX").unwrap_or_else(|_| "pulse-ci-runner-".into());
        let cap_gib = std::env::var("PITWALL_SLICE_CAP_GIB")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(24);
        Config {
            socket_path,
            repo,
            prefix,
            slice_cap_bytes: cap_gib * 1024 * 1024 * 1024,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cap_gib_converts_to_bytes_default_24() {
        // Defaults hold when env is unset in the test process.
        std::env::remove_var("PITWALL_SLICE_CAP_GIB");
        std::env::remove_var("PITWALL_REPO");
        std::env::remove_var("PITWALL_PREFIX");
        let c = Config::from_env();
        assert_eq!(c.slice_cap_bytes, 24 * 1024 * 1024 * 1024);
        assert_eq!(c.repo, "erwins-enkel/pulse");
        assert_eq!(c.prefix, "pulse-ci-runner-");
    }

    #[test]
    fn socket_path_falls_back_to_real_uid() {
        std::env::remove_var("PITWALL_SOCKET");
        std::env::remove_var("DOCKER_HOST");
        let uid = unsafe { libc::getuid() };
        assert_eq!(
            Config::from_env().socket_path,
            format!("/run/user/{uid}/docker.sock")
        );
    }

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
