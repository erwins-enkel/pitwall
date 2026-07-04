#[derive(Clone)]
pub struct Config {
    pub socket_path: String,
    pub repo: String,
    pub prefix: String,
    pub slice_cap_bytes: u64,
}

impl Config {
    pub fn from_env() -> Config {
        let socket_path = std::env::var("PITWALL_SOCKET").ok().unwrap_or_else(|| {
            std::env::var("DOCKER_HOST")
                .ok()
                .map(|h| h.trim_start_matches("unix://").to_string())
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
}
