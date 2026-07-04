// Consumed by `resource`, `jobs`, and `ui` in later tasks; not yet wired into `main`.
#[allow(dead_code)]
mod config;

#[allow(dead_code)]
mod model;

#[allow(dead_code)]
mod stats_math;

// `resource::run` has no caller until Task 7 wires it into the event loop.
#[allow(dead_code)]
mod resource;

// `jobs::run` has no caller until Task 7 wires it into the event loop.
#[allow(dead_code)]
mod jobs;

// `ui::render` has no caller until Task 7 wires it into the event loop.
#[allow(dead_code)]
mod ui;

fn main() {
    println!("pitwall");
}
