//! embedded-debug-mcp — shared library for MCP server and dutabo CLI.
//!
//! Public API for use by binaries (`main.rs`, `bin/dutabo.rs`).

pub mod boot_detector;
pub mod command_queue;
pub mod config;
pub mod connection_learner;
pub mod console;
pub mod flash;
pub mod inotify_watcher;
pub mod lock_manager;
pub mod log_manager;
pub mod marker;
pub mod mcp;
pub mod mcp_http;
pub use dut_ctrl as power_control;
pub mod relay_manager;
pub mod serial_engine;
pub mod state_manager;
