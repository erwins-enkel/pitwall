mod app;
mod config;
mod jobs;
mod model;
mod resource;
mod stats_math;
mod ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ratatui::init installs a panic hook that restores the terminal before unwinding.
    let terminal = ratatui::init();
    let res = app::run(terminal).await;
    ratatui::restore();
    res
}
