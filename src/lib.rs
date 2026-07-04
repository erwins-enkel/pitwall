//! Library crate exposing pitwall's modules so both the binary (`main.rs`) and
//! generators like `examples/screenshot.rs` can drive the real render path.
pub mod app;
pub mod config;
pub mod history;
pub mod jobs;
pub mod model;
pub mod resource;
pub mod resource_docker;
pub mod resource_native;
pub mod stats_math;
pub mod theme;
pub mod ui;
