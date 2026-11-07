//! Drives the built binary to check flag handling. Every case here exits during
//! arg parsing, before the terminal is touched, so no TTY is needed.
use std::process::{Command, Output};

fn pitwall(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pitwall"))
        .args(args)
        .output()
        .expect("spawn pitwall")
}

#[test]
fn version_prints_name_and_version() {
    for flag in ["--version", "-V"] {
        let out = pitwall(&[flag]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("pitwall {}\n", env!("CARGO_PKG_VERSION")),
            "{flag}"
        );
    }
}

#[test]
fn help_lists_keys_env_vars_and_config_path() {
    for flag in ["--help", "-h"] {
        let out = pitwall(&[flag]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        for needle in [
            "q, Esc, Ctrl-C",
            "PITWALL_SOCKET",
            "PITWALL_REPO",
            "PITWALL_PREFIX",
            "PITWALL_SLICE_CAP_GIB",
            "PITWALL_THEME",
            "PITWALL_MEM_WARN_PCT",
            "PITWALL_MEM_CRIT_PCT",
            "PITWALL_CONFIG",
            "pitwall/config.toml",
        ] {
            assert!(stdout.contains(needle), "{flag} missing {needle:?}");
        }
    }
}

#[test]
fn help_ignores_broken_config() {
    let out = Command::new(env!("CARGO_BIN_EXE_pitwall"))
        .arg("--help")
        .env("PITWALL_CONFIG", "/nonexistent/pitwall.toml")
        .output()
        .expect("spawn pitwall");
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn unknown_args_exit_2_without_drawing() {
    for arg in ["--bogus", "foo"] {
        let out = pitwall(&[arg]);
        assert_eq!(out.status.code(), Some(2), "{arg}");
        assert!(!out.stderr.is_empty(), "{arg}: no error on stderr");
        assert!(out.stdout.is_empty(), "{arg}: wrote to stdout");
    }
}
