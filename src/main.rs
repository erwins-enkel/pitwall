mod app;
mod config;
mod jobs;
mod model;
mod resource;
mod stats_math;
mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load config before entering the alternate screen so a bad config file
    // reports cleanly to stderr instead of being swallowed by the TUI.
    let cfg = config::Config::load()?;
    // ratatui::init installs a panic hook that restores the terminal before unwinding.
    let terminal = ratatui::init();
    let res = app::run(terminal, cfg).await;
    ratatui::restore();
    res
}
