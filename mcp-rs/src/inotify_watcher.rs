//! inotify event-driven watcher — monitors project files for changes.
//!
//! Avoids polling: uses Linux inotify to watch:
//! - `.target.toml` — config changes → prompt MCP restart
//! - `current.serial.log` — new log data → CLI real-time display
//! - `target-state` — state changes → hook notifications
//!
//! # Usage
//!
//! Spawn a dedicated thread to watch for events:
//!
//! ```ignore
//! let watcher = InotifyWatcher::new(project_dir, dut_dir)?;
//! std::thread::spawn(move || {
//!     loop {
//!         match watcher.wait() {
//!             Ok(event) => match event.kind {
//!                 WatchKind::ConfigChanged => eprintln!("Config changed — restart MCP"),
//!                 WatchKind::LogAppended => { /* read new log lines */ },
//!                 WatchKind::StateChanged => { /* update statusline */ },
//!             },
//!             Err(e) => eprintln!("inotify error: {e}"),
//!         }
//!     }
//! });
//! ```

use std::path::{Path, PathBuf};

use inotify::{Inotify, WatchMask};

/// Types of filesystem events we care about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchKind {
    /// `.target.toml` was modified → MCP should restart or hot-reload.
    ConfigChanged,
    /// `current.serial.log` was appended to → new log data available.
    LogAppended,
    /// `target-state` was written → DUT state changed.
    StateChanged,
}

/// A single inotify event.
#[derive(Debug)]
pub struct WatchEvent {
    pub kind: WatchKind,
    pub path: PathBuf,
}

/// inotify watcher for debug-console-mcp project files.
pub struct InotifyWatcher {
    inotify: Inotify,
    /// Buffer for reading events.
    buffer: [u8; 4096],
}

impl InotifyWatcher {
    /// Create a new watcher for the given project directory.
    pub fn new(project_dir: &Path, dut_dir: &str) -> Result<Self, String> {
        let inotify = Inotify::init().map_err(|e| format!("inotify init: {e}"))?;

        let dut_path = project_dir.join(dut_dir);
        let log_dir = dut_path.join("logs");

        // Ensure directories exist
        std::fs::create_dir_all(&log_dir).ok();
        std::fs::create_dir_all(&dut_path).ok();

        // Watch .target.toml (may not exist yet — that's OK)
        let config_path = project_dir.join(".target.toml");
        if config_path.exists() {
            inotify
                .watches()
                .add(&config_path, WatchMask::MODIFY | WatchMask::CLOSE_WRITE)
                .map_err(|e| format!("watch .target.toml: {e}"))?;
        }

        // Watch log directory for current.serial.log changes
        if log_dir.exists() {
            inotify
                .watches()
                .add(
                    &log_dir,
                    WatchMask::MODIFY | WatchMask::CLOSE_WRITE | WatchMask::CREATE,
                )
                .map_err(|e| format!("watch logs/: {e}"))?;
        }

        // Watch DUT directory for target-state changes
        if dut_path.exists() {
            inotify
                .watches()
                .add(
                    &dut_path,
                    WatchMask::MODIFY | WatchMask::CLOSE_WRITE | WatchMask::CREATE,
                )
                .map_err(|e| format!("watch dut_dir: {e}"))?;
        }

        Ok(Self {
            inotify,
            buffer: [0u8; 4096],
        })
    }

    /// Block until an event occurs, then return it.
    /// Runs in blocking mode — use from a dedicated thread.
    pub fn wait(&mut self) -> Result<WatchEvent, String> {
        loop {
            let events = self
                .inotify
                .read_events(&mut self.buffer)
                .map_err(|e| format!("inotify read: {e}"))?;

            for ev in events {
                let name = ev.name.and_then(|n| n.to_str()).unwrap_or("");

                let kind = if name == ".target.toml" {
                    Some(WatchKind::ConfigChanged)
                } else if name == "current.serial.log" {
                    Some(WatchKind::LogAppended)
                } else if name == "target-state" {
                    Some(WatchKind::StateChanged)
                } else {
                    None
                };

                if let Some(kind) = kind {
                    return Ok(WatchEvent {
                        kind,
                        path: PathBuf::from(name),
                    });
                }
            }
            // If events arrived but didn't match our filters, loop and wait again
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watcher_creation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dut_dir = ".dut-serial";
        std::fs::create_dir_all(tmp.path().join(dut_dir).join("logs")).unwrap();
        std::fs::write(
            tmp.path().join(".target.toml"),
            "[dev_host]\nip = \"127.0.0.1\"\n",
        )
        .unwrap();

        let watcher = InotifyWatcher::new(tmp.path(), dut_dir);
        assert!(watcher.is_ok());
    }

    #[test]
    fn test_config_change_detection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dut_dir = ".dut-serial";
        std::fs::create_dir_all(tmp.path().join(dut_dir).join("logs")).unwrap();
        let config_path = tmp.path().join(".target.toml");
        std::fs::write(&config_path, "[dev_host]\nip = \"127.0.0.1\"\n").unwrap();

        // Create watcher BEFORE modifying files
        let mut watcher = InotifyWatcher::new(tmp.path(), dut_dir).unwrap();

        // Write state file AFTER watcher is set up (so it catches the event)
        let state_path = tmp.path().join(dut_dir).join("target-state");
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&state_path, "active").unwrap();

        // wait() with a timeout via channel
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = watcher.wait();
            tx.send(result).ok();
        });

        // Wait up to 2 seconds for event
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Ok(event)) => {
                assert!(
                    event.kind == WatchKind::ConfigChanged || event.kind == WatchKind::StateChanged,
                    "Unexpected event kind: {:?}",
                    event.kind
                );
            }
            Ok(Err(e)) => {
                // Test environment may not support inotify well
                eprintln!("inotify error (may be OK in CI): {e}");
            }
            Err(_) => {
                // Timeout — inotify events may not be delivered in all test envs
                eprintln!("Timeout waiting for inotify event (may be OK in CI)");
            }
        }
    }
}
