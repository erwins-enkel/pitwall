use clap::Parser;
use pitwall::{app, config};

/// Extra `--help` text: quit keys, env vars, and config path. Defaults live in
/// the README's Configuration table (kept out of here so they can't drift).
const HELP_AFTER: &str = "\
Keys:
  q, Esc, Ctrl-C  Quit (terminal is restored on exit)

Environment (take precedence over the config file; empty = unset):
  PITWALL_SOCKET         Docker socket path
  PITWALL_REPO           Repo(s) to poll, comma-separated owner/repo
  PITWALL_PREFIX         Runner container name prefix(es), optionally prefix=owner/repo
  PITWALL_SLICE_CAP_GIB  Memory cap for the docker memory gauge (GiB)
  PITWALL_THEME          Catppuccin flavor: mocha, macchiato, frappe, latte
  PITWALL_MEM_WARN_PCT   Memory warn threshold (%)
  PITWALL_MEM_CRIT_PCT   Memory critical threshold (%)
  PITWALL_CONFIG         Config file path override

Config file:
  $XDG_CONFIG_HOME/pitwall/config.toml (fallback ~/.config/pitwall/config.toml)

See the README for defaults and details.";

/// btop-like terminal UI for self-hosted GitHub Actions runners.
#[derive(Parser)]
#[command(version, after_help = HELP_AFTER)]
struct Cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse args first: --help/--version/unknown flags exit here, before the
    // config is loaded or anything is drawn.
    Cli::parse();
    // Load config before entering the alternate screen so a bad config file
    // reports cleanly to stderr instead of being swallowed by the TUI.
    let cfg = config::Config::load()?;
    // ratatui::init installs a panic hook that restores the terminal before unwinding.
    let terminal = ratatui::init();
    let res = app::run(terminal, cfg).await;
    ratatui::restore();
    res
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
