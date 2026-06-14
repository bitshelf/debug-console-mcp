//! State manager — hysteresis-debounced finite state machine with three-tier
//! visibility (internal → MCP API → statusline file).
//!
//! # State diagram
//!
//! ```text
//!                    ┌──────────┐
//!                    │  Stopped │  (internal — engine not started)
//!                    └────┬─────┘
//!                         │ start()
//!                    ┌────▼─────┐
//!                    │Connecting│  (internal — TCP handshake in progress)
//!                    └────┬─────┘
//!                    ┌────┴──────────────┐
//!                    │                   │
//!               ┌────▼─────┐       ┌─────▼──────┐
//!               │  Active  │◄──────│Disconnected │  ✗ visible
//!               └──┬───┬───┘       └─────┬──────┘
//!                  │   │                 │
//!      ┌───────────┘   └──────────┐      │
//!      │              reconnect() │      │ read error /
//!      │                          │      │ connect fail
//! ┌────▼─────┐               ┌────▼─────┐
//! │ Booting  │──────────────►│  UBoot   │  ◐/● visible
//! └────┬─────┘               └────┬─────┘
//!      │                          │
//!      │ kernel panic             │ Ctrl-C interrupt
//! ┌────▼─────┐                    │
//! │ Crashed  │◄───────────────────┘  ✗ visible
//! └────┬─────┘
//!      │ (watchdog reboot)
//!      └──────► Booting → ...
//!
//! ┌──────────┐
//! │  DUT-off │  ✗ visible  (heartbeat timeout / no probe response)
//! └────┬─────┘
//!      │ (data arrives) → Active
//!      │ (read error)   → Disconnected
//!
//! ┌──────────┐
//! │  Dutabo  │  ● visible  (serial taken over by dutabo CLI)
//! └──────────┘
//!      │ (dutabo exits) → reconnect → probe → Active
//! ```
//!
//! # Event sources and transitions
//!
//! | Event | Source | Old → New |
//! |-------|--------|-----------|
//! | TCP connect OK + probe finds shell/login | `probe_initial_state()` | any → Active |
//! | SPL/DDR detected | `BootStageDetector::feed()` | any → Booting |
//! | U-Boot prompt detected | `BootStageDetector::feed()` | Booting → UBoot |
//! | Kernel panic / BUG / Oops | `BootStageDetector::feed()` | any → Crashed |
//! | Heartbeat timeout (60s + 3× hysteresis) | `check_hang()` | Active/Booting → DUT-off |
//! | Blank-line probe returned data | `read_loop_iter()` | DUT-off → Active |
//! | TCP read error ("Connection closed") | `handle_read_error()` | any → Disconnected |
//! | TCP connect fails (backoff retry) | `read_loop_iter()` | Disconnected → Disconnected |
//! | TCP connect succeeds after backoff | `read_loop_iter()` | Disconnected → *probed* |
//! | dutabo sentinel file created | `read_loop_iter()` | any → Dutabo |
//! | dutabo sentinel removed | `read_loop_iter()` | Dutabo → *reconnected* |
//!
//! # Hysteresis / debounce
//!
//! - **Hang detection** (`check_hang`): state must be in hang-candidate set
//!   (Active, Booting, Dutabo). `hang_count` increments each 2s watchdog tick
//!   with no data. Must reach `hysteresis` (default 3 → 6s) before DUT-off.
//! - **Heartbeat probes**: Active state with no data for `hang_timeout` (60s)
//!   triggers a blank-line probe. If no response within 5s, `heartbeat_missed++`.
//!   After `hysteresis` misses → DUT-off.
//! - **Crash throttle**: crash events within 2s of each other are suppressed
//!   (a single panic often produces multiple log lines matching crash patterns).
//!
//! # Visibility tiers
//!
//! | Tier | States visible | Consumers |
//! |------|---------------|-----------|
//! | Internal | All 10 states | `serial_engine.rs` logic |
//! | MCP API | 7 states (excl. Stopped, Connecting, Booted) | `serial_get_state` tool |
//! | Statusline | 6 states (excl. Stopped, Connecting, Booted, Disconnected) | `statusline.py` hook |
//!
//! `Disconnected` writes the state file but **deletes** the statusline cache,
//! so `serial_get_state` reports it but the user's terminal does not flicker.

use std::path::{Path, PathBuf};
use std::time::Instant;

/// Target board state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    Stopped,
    #[allow(dead_code)]
    Connecting,
    Active,
    Booting,
    #[allow(dead_code)]
    Booted,
    UBoot,
    Crashed,
    DutOff,
    Disconnected,
    /// Serial is taken over by dutabo interactive session
    Dutabo,
}

impl TargetState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Connecting => "connecting",
            Self::Active => "active",
            Self::Booting => "booting",
            Self::Booted => "booted",
            Self::UBoot => "uboot",
            Self::Crashed => "crashed",
            Self::DutOff => "DUT-off",
            Self::Disconnected => "disconnected",
            Self::Dutabo => "dutabo",
        }
    }

    #[allow(dead_code)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "stopped" => Self::Stopped,
            "connecting" => Self::Connecting,
            "active" => Self::Active,
            "booting" => Self::Booting,
            "booted" => Self::Booted,
            "uboot" => Self::UBoot,
            "crashed" => Self::Crashed,
            "DUT-off" => Self::DutOff,
            "disconnected" => Self::Disconnected,
            "dutabo" => Self::Dutabo,
            _ => Self::Disconnected,
        }
    }

    /// MCP-visible states (excludes stopped/connecting).
    pub fn is_external(&self) -> bool {
        !matches!(self, Self::Stopped | Self::Connecting)
    }

    /// Hang/heartbeat detection candidates: booting + active.
    /// booting: long silence → hang
    /// active:  long silence → send heartbeat probe to check liveness
    fn is_hang_candidate(&self) -> bool {
        matches!(self, Self::Booting | Self::Active | Self::Dutabo)
    }
}

/// Write a file with restricted permissions (0600) to prevent other users
/// from reading potentially sensitive target information.
fn write_secure(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    std::fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Manages target device state with hysteresis debounce, atomic file writes,
/// hang/heartbeat detection, and three-tier state visibility (internal, MCP-API, statusline).
pub struct StateManager {
    current: TargetState,
    state_file: PathBuf,
    /// Project-local cache (fallback, read by Python hook)
    cache_file: PathBuf,
    /// /dev/shm cache — zero-syscall read by Python hook via `cat`
    shm_cache_file: PathBuf,
    /// Notification directory for Agent proactive alerts
    notify_dir: PathBuf,
    hang_timeout_secs: u64,
    hysteresis: u32,
    hang_count: u32,
    last_data_time: Instant,
    /// Heartbeat probe: set to true when active state has no data, triggers
    /// serial_engine to send a probe.
    pub heartbeat_pending: bool,
    /// Time of last probe sent (gives the board a response window).
    last_probe_time: Instant,
    /// Number of consecutive probe misses (exceeds hysteresis → DUT-off).
    heartbeat_missed: u32,
    /// DUT alias for per-DUT state file paths (empty = single DUT, uses root .dut-serial/).
    dut_alias: String,
    /// Project root directory (for constructing alias-specific state paths).
    project_dir: PathBuf,
    /// Metrics: total commands successfully executed.
    command_count: u64,
    /// Metrics: total command errors (timeout, disconnection).
    error_count: u64,
    /// Metrics: engine start timestamp.
    start_time: Instant,
    /// Metrics: command latencies in ms (circular buffer, max 1000 samples).
    latencies_ms: Vec<u64>,
}

impl StateManager {
    pub fn new(
        project_dir: &Path,
        hang_timeout: u64,
        hysteresis: u32,
        dut_dir: &str,
        dut_alias: &str,
    ) -> Self {
        let dut_dir_path = project_dir.join(dut_dir);
        let state_file = dut_dir_path.join("target-state");
        let cache_file = dut_dir_path.join("statusline-cache");
        let notify_dir = dut_dir_path.join("notifications");

        // /dev/shm cache: keyed by project path hash so multiple projects don't collide
        let shm_dir = if std::path::Path::new("/dev/shm").is_dir() {
            "/dev/shm"
        } else {
            "/tmp"
        };
        let project_hash = Self::project_hash(project_dir);
        let shm_cache_file =
            std::path::PathBuf::from(format!("{}/claude-status-{}", shm_dir, project_hash));

        std::fs::create_dir_all(&dut_dir_path).ok();
        std::fs::create_dir_all(&notify_dir).ok();

        Self {
            current: TargetState::Stopped,
            state_file,
            cache_file,
            shm_cache_file,
            notify_dir,
            hang_timeout_secs: hang_timeout,
            hysteresis,
            hang_count: 0,
            last_data_time: Instant::now(),
            heartbeat_pending: false,
            last_probe_time: Instant::now(),
            heartbeat_missed: 0,
            dut_alias: dut_alias.to_string(),
            project_dir: project_dir.to_path_buf(),
            command_count: 0,
            error_count: 0,
            start_time: Instant::now(),
            latencies_ms: Vec::new(),
        }
    }

    /// Stable 8-char hex hash matching Python's get_session_id() (md5).
    /// Both MCP and statusline hook use the same project_dir → same hash.
    fn project_hash(project_dir: &Path) -> String {
        use md5::{Digest, Md5};
        let canonical = project_dir
            .canonicalize()
            .unwrap_or_else(|_| project_dir.to_path_buf());
        let mut hasher = Md5::new();
        hasher.update(canonical.to_string_lossy().as_bytes());
        let digest = hasher.finalize();
        format!("{:08x}", digest).chars().take(8).collect()
    }

    /// Write PID file with restricted permissions (0600).
    /// Call only after the project-level lock is acquired.
    pub fn write_pid(&self, project_dir: &Path, dut_dir: &str) {
        let pid_file = project_dir.join(dut_dir).join("mcp.pid");
        let _ = write_secure(&pid_file, &std::process::id().to_string());
    }

    /// Return the current internal target state.
    pub fn current(&self) -> TargetState {
        self.current
    }

    /// MCP API state (stopped/connecting → None)
    pub fn external_state(&self) -> Option<TargetState> {
        if self.current.is_external() {
            Some(self.current)
        } else {
            None
        }
    }

    /// Increment the successful command counter.
    pub fn inc_command(&mut self) {
        self.command_count += 1;
    }

    /// Increment the command error counter.
    pub fn inc_error(&mut self) {
        self.error_count += 1;
    }

    /// Engine uptime in seconds since start.
    pub fn uptime_secs(&self) -> f64 {
        self.start_time.elapsed().as_secs_f64()
    }

    /// Number of commands successfully executed.
    pub fn command_count(&self) -> u64 {
        self.command_count
    }

    /// Number of command errors (timeout, disconnection).
    pub fn error_count(&self) -> u64 {
        self.error_count
    }

    /// Record a command latency in milliseconds.
    pub fn record_latency(&mut self, ms: u64) {
        if self.latencies_ms.len() >= 1000 {
            self.latencies_ms.remove(0);
        }
        self.latencies_ms.push(ms);
    }

    /// Compute p50/p95/p99 latency from recorded samples.
    pub fn latency_percentiles(&self) -> (u64, u64, u64) {
        if self.latencies_ms.is_empty() {
            return (0, 0, 0);
        }
        let mut sorted = self.latencies_ms.clone();
        sorted.sort_unstable();
        let p50 = sorted[(sorted.len() - 1) * 50 / 100];
        let p95 = sorted[(sorted.len() - 1) * 95 / 100];
        let p99 = sorted[(sorted.len() - 1) * 99 / 100];
        (p50, p95, p99)
    }

    /// Transition to a new target state. No-op if `new` is the same as current.
    /// Writes state files atomically for external states; deletes files on Stopped;
    /// skips file writes for Connecting (avoiding statusline flicker).
    pub fn transition(&mut self, new: TargetState) {
        if new == self.current {
            return;
        }
        // Warn on suspicious transitions — don't block, the external world
        // can produce unexpected state sequences (e.g. board watchdog reboot
        // during a crash). These warnings help diagnose bugs in the detector.
        let from = self.current;
        match (from, new) {
            // Crashed → Active without an intervening boot is suspicious.
            (TargetState::Crashed, TargetState::Active) => {
                tracing::warn!("Suspicious transition: crashed → active (no boot cycle detected)");
            }
            // Dutabo takeover should come from a connected state.
            (TargetState::Disconnected, TargetState::Dutabo) => {
                tracing::warn!("Suspicious transition: disconnected → dutabo");
            }
            _ => {}
        }
        tracing::info!("StateManager: {} → {}", from.as_str(), new.as_str());
        self.current = new;

        match new {
            TargetState::Stopped => {
                self.delete_state_file();
                self.delete_cache_file();
                self.delete_shm_cache();
            }
            TargetState::Connecting => {
                // Don't write file — avoid statusline flicker
            }
            TargetState::Active
            | TargetState::Booting
            | TargetState::Booted
            | TargetState::UBoot
            | TargetState::Crashed
            | TargetState::Disconnected
            | TargetState::DutOff
            | TargetState::Dutabo => {
                self.atomic_write(&self.state_file, new.as_str());
                // Also write to per-DUT alias directory when alias is configured
                if !self.dut_alias.is_empty() {
                    let alias_state_file = self
                        .project_dir
                        .join(".dut-serial")
                        .join(&self.dut_alias)
                        .join("target-state");
                    self.atomic_write(&alias_state_file, new.as_str());
                }
                let text = self.format_statusline(new);
                self.atomic_write(&self.cache_file, &text);
                self.atomic_write(&self.shm_cache_file, &text);
                // Also write per-DUT cache when alias is configured
                if !self.dut_alias.is_empty() && self.dut_alias != "default" {
                    let alias_cache = self
                        .project_dir
                        .join(".dut-serial")
                        .join(&self.dut_alias)
                        .join("statusline-cache");
                    self.atomic_write(&alias_cache, &text);
                }
                // Write Agent notification for critical states
                if matches!(
                    new,
                    TargetState::Crashed | TargetState::DutOff | TargetState::Disconnected
                ) {
                    self.write_notification(new);
                }
            }
        }
    }

    /// Produce an ANSI-formatted statusline string for a given target state.
    /// Format a statusline string with ANSI color. Uses the DUT alias
    /// from .target.toml if available, otherwise "serial".
    pub(crate) fn format_statusline(&self, state: TargetState) -> String {
        let label = if self.dut_alias.is_empty() || self.dut_alias == "default" {
            "serial"
        } else {
            &self.dut_alias
        };
        Self::format_statusline_labeled(state, label)
    }

    /// Format statusline with explicit label (for testing, or when no
    /// StateManager instance is available).
    pub(crate) fn format_statusline_labeled(state: TargetState, label: &str) -> String {
        match state {
            TargetState::Active => format!("\x1b[32m● {}:active\x1b[0m", label),
            TargetState::Booting => format!("\x1b[33m◐ {}:booting\x1b[0m", label),
            TargetState::Booted => format!("\x1b[32m● {}:booted\x1b[0m", label),
            TargetState::UBoot => format!("\x1b[36m● {}:uboot\x1b[0m", label),
            TargetState::Crashed => format!("\x1b[31m✗ {}:crashed\x1b[0m", label),
            TargetState::Disconnected => format!("\x1b[31m✗ {}:disconnected\x1b[0m", label),
            TargetState::DutOff => format!("\x1b[31m✗ {}:DUT-off\x1b[0m", label),
            TargetState::Dutabo => format!("\x1b[35m● {}:dutabo\x1b[0m", label),
            TargetState::Stopped | TargetState::Connecting => String::new(),
        }
    }

    /// Called on every serial data arrival — resets hang and heartbeat counters.
    /// Also resets `last_probe_time` so the next heartbeat cycle starts fresh.
    pub fn on_activity(&mut self) {
        self.last_data_time = Instant::now();
        self.last_probe_time = Instant::now();
        self.hang_count = 0;
        self.heartbeat_pending = false;
        self.heartbeat_missed = 0;
    }

    /// Mark that a heartbeat probe was sent (called by serial_engine).
    pub fn mark_probe_sent(&mut self) {
        self.last_probe_time = Instant::now();
        self.heartbeat_pending = false;
    }

    /// Check for hang / heartbeat timeout.
    /// booting: long silence → hang_count++ → DUT-off
    /// active:  long silence → send newline probe (no command);
    ///          if no response within 5s → miss++ → DUT-off
    pub fn check_hang(&mut self) {
        if !self.current.is_hang_candidate() {
            self.hang_count = 0;
            self.heartbeat_pending = false;
            return;
        }
        let data_elapsed = self.last_data_time.elapsed().as_secs_f64();
        if self.current == TargetState::Active {
            let probe_elapsed = self.last_probe_time.elapsed().as_secs_f64();
            // Probe sent and no response within 5s window
            if probe_elapsed > 5.0 && data_elapsed > self.hang_timeout_secs as f64 {
                self.heartbeat_missed += 1;
                tracing::warn!(
                    "Heartbeat miss #{}: no data for {:.0}s, probe sent {:.0}s ago",
                    self.heartbeat_missed,
                    data_elapsed,
                    probe_elapsed
                );
                if self.heartbeat_missed >= self.hysteresis {
                    tracing::warn!("Heartbeat: {} misses → DUT-off", self.heartbeat_missed);
                    self.transition(TargetState::DutOff);
                } else {
                    // Request another probe
                    self.heartbeat_pending = true;
                }
            } else if data_elapsed > self.hang_timeout_secs as f64
                && probe_elapsed > self.hang_timeout_secs as f64
            {
                // First timeout — request a probe
                self.heartbeat_pending = true;
            }
        } else {
            // Booting state — no output timeout → hang
            if data_elapsed > self.hang_timeout_secs as f64 {
                self.hang_count += 1;
                if self.hang_count >= self.hysteresis {
                    self.transition(TargetState::DutOff);
                }
            } else {
                self.hang_count = 0;
            }
        }
    }

    pub fn last_data_elapsed(&self) -> std::time::Duration {
        self.last_data_time.elapsed()
    }

    /// Write a notification JSON file for Agent proactive alerts.
    /// Notifications are written to `.dut-serial/notifications/<timestamp>-<state>.json`.
    fn write_notification(&self, state: TargetState) {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let fname = format!("{ts}-{}.json", state.as_str());
        let path = self.notify_dir.join(&fname);

        let alert = match state {
            TargetState::Crashed => serde_json::json!({
                "type": "target_alert",
                "state": "crashed",
                "severity": "critical",
                "message": "Target has crashed (Kernel panic/BUG/Oops detected). Check serial logs for crash details.",
                "suggested_action": "Use serial_get_logs with pattern='panic|BUG|Oops' to analyze the crash.",
                "timestamp": chrono::Local::now().to_rfc3339(),
            }),
            TargetState::DutOff => serde_json::json!({
                "type": "target_alert",
                "state": "DUT-off",
                "severity": "warning",
                "message": "Target is not responding (no serial output). May be powered off or hung.",
                "suggested_action": "Try serial_reset to reboot the target, or check power supply.",
                "timestamp": chrono::Local::now().to_rfc3339(),
            }),
            TargetState::Disconnected => serde_json::json!({
                "type": "target_alert",
                "state": "disconnected",
                "severity": "warning",
                "message": "Serial connection lost. ser2net may be down or network issue.",
                "suggested_action": "Check ser2net on dev host, or use serial_claim to reconnect.",
                "timestamp": chrono::Local::now().to_rfc3339(),
            }),
            _ => return,
        };

        if let Err(e) = write_secure(
            &path,
            &serde_json::to_string_pretty(&alert).unwrap_or_default(),
        ) {
            tracing::error!("Failed to write notification {path:?}: {e}");
        } else {
            tracing::info!("Agent notification written: {fname}");
            // Also append to a consolidated alert log
            let alert_log = self.notify_dir.join("alerts.log");
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&alert_log)
                .ok();
            if let Some(ref mut file) = f {
                use std::io::Write;
                writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&alert).unwrap_or_default()
                )
                .ok();
            }
        }
    }

    fn atomic_write(&self, path: &Path, content: &str) {
        let tmp = path.with_extension("tmp");
        if let Err(e) = write_secure(&tmp, content) {
            tracing::error!("StateManager: write failed: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            tracing::error!("StateManager: rename failed: {e}");
        }
    }

    fn delete_state_file(&self) {
        let _ = std::fs::remove_file(&self.state_file);
    }

    fn delete_cache_file(&self) {
        let _ = std::fs::remove_file(&self.cache_file);
    }

    fn delete_shm_cache(&self) {
        let _ = std::fs::remove_file(&self.shm_cache_file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_state_manager(hang_timeout: u64, hysteresis: u32) -> (StateManager, TempDir) {
        let tmp = TempDir::new().unwrap();
        let sm = StateManager::new(tmp.path(), hang_timeout, hysteresis, ".dut-serial", "");
        (sm, tmp)
    }

    #[test]
    fn test_initial_state() {
        let (sm, _tmp) = create_test_state_manager(60, 3);
        assert_eq!(sm.current(), TargetState::Stopped);
        assert_eq!(sm.external_state(), None); // Stopped is not external
    }

    #[test]
    fn test_state_transitions() {
        let (mut sm, _tmp) = create_test_state_manager(60, 3);

        sm.transition(TargetState::Active);
        assert_eq!(sm.current(), TargetState::Active);
        assert_eq!(sm.external_state(), Some(TargetState::Active));

        sm.transition(TargetState::Booting);
        assert_eq!(sm.current(), TargetState::Booting);
        assert_eq!(sm.external_state(), Some(TargetState::Booting));

        sm.transition(TargetState::Booted);
        assert_eq!(sm.current(), TargetState::Booted);
    }

    #[test]
    fn test_connecting_not_external() {
        let (mut sm, _tmp) = create_test_state_manager(60, 3);
        sm.transition(TargetState::Connecting);
        assert_eq!(sm.current(), TargetState::Connecting);
        assert_eq!(sm.external_state(), None);
    }

    #[test]
    fn test_state_file_written() {
        let (mut sm, tmp) = create_test_state_manager(60, 3);
        let state_file = tmp.path().join(".dut-serial/target-state");

        sm.transition(TargetState::Active);
        assert_eq!(std::fs::read_to_string(&state_file).unwrap(), "active");

        sm.transition(TargetState::Booted);
        assert_eq!(std::fs::read_to_string(&state_file).unwrap(), "booted");

        sm.transition(TargetState::Crashed);
        assert_eq!(std::fs::read_to_string(&state_file).unwrap(), "crashed");
    }

    #[test]
    fn test_connecting_no_file_write() {
        let (mut sm, tmp) = create_test_state_manager(60, 3);
        let state_file = tmp.path().join(".dut-serial/target-state");

        // First transition to something that writes
        sm.transition(TargetState::Active);
        assert_eq!(std::fs::read_to_string(&state_file).unwrap(), "active");

        // Connecting should not update file
        sm.transition(TargetState::Connecting);
        assert_eq!(std::fs::read_to_string(&state_file).unwrap(), "active");
    }

    #[test]
    fn test_hang_detection_not_candidate() {
        let (mut sm, _tmp) = create_test_state_manager(1, 2);

        // Active is a candidate now, but without timeout it stays Active
        sm.transition(TargetState::Active);
        sm.check_hang();
        assert_eq!(sm.current(), TargetState::Active);

        // Booted/UBoot/Crashed/Disconnected are NOT candidates
        sm.transition(TargetState::Booted);
        sm.check_hang();
        assert_eq!(sm.current(), TargetState::Booted);
    }

    #[test]
    fn test_hang_detection_booting() {
        let (mut sm, _tmp) = create_test_state_manager(0, 2); // 0s timeout for fast test

        // Small sleep to ensure elapsed > 0 (as_secs_f64() allows sub-second precision)
        std::thread::sleep(std::time::Duration::from_millis(50));
        sm.transition(TargetState::Booting);
        assert_eq!(sm.current(), TargetState::Booting);

        // First check increments hang_count but doesn't transition (need hysteresis=2)
        sm.check_hang();
        assert_eq!(sm.current(), TargetState::Booting);

        // Second check should trigger transition
        sm.check_hang();
        assert_eq!(sm.current(), TargetState::DutOff);
    }

    #[test]
    fn test_activity_resets_hang() {
        let (mut sm, _tmp) = create_test_state_manager(0, 3);

        sm.transition(TargetState::Booting);
        std::thread::sleep(std::time::Duration::from_millis(100));

        sm.check_hang(); // hang_count = 1
        sm.on_activity(); // resets hang_count to 0
        sm.check_hang(); // hang_count = 1 again
        sm.check_hang(); // hang_count = 2

        // Should not transition yet (need 3)
        assert_eq!(sm.current(), TargetState::Booting);
    }

    #[test]
    fn test_pid_file_write() {
        let tmp = TempDir::new().unwrap();
        let sm = StateManager::new(tmp.path(), 60, 3, ".dut-serial", "");
        // PID not written in new() anymore — only after lock
        let pid_file = tmp.path().join(".dut-serial/mcp.pid");
        assert!(!pid_file.exists());
        // Now write it
        sm.write_pid(tmp.path(), ".dut-serial");
        assert!(pid_file.exists());
        let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().parse().unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn test_target_state_as_str() {
        assert_eq!(TargetState::Stopped.as_str(), "stopped");
        assert_eq!(TargetState::Connecting.as_str(), "connecting");
        assert_eq!(TargetState::Active.as_str(), "active");
        assert_eq!(TargetState::Booting.as_str(), "booting");
        assert_eq!(TargetState::Booted.as_str(), "booted");
        assert_eq!(TargetState::UBoot.as_str(), "uboot");
        assert_eq!(TargetState::Crashed.as_str(), "crashed");
        assert_eq!(TargetState::DutOff.as_str(), "DUT-off");
        assert_eq!(TargetState::Disconnected.as_str(), "disconnected");
    }

    #[test]
    fn test_target_state_from_str() {
        assert_eq!(TargetState::from_str("stopped"), TargetState::Stopped);
        assert_eq!(TargetState::from_str("active"), TargetState::Active);
        assert_eq!(TargetState::from_str("DUT-off"), TargetState::DutOff);
        assert_eq!(TargetState::from_str("unknown"), TargetState::Disconnected);
    }

    #[test]
    fn test_hang_candidate() {
        assert!(!TargetState::Stopped.is_hang_candidate());
        assert!(!TargetState::Connecting.is_hang_candidate());
        assert!(TargetState::Active.is_hang_candidate()); // heartbeat probe
        assert!(TargetState::Booting.is_hang_candidate());
        assert!(!TargetState::Booted.is_hang_candidate());
        assert!(!TargetState::UBoot.is_hang_candidate());
        assert!(!TargetState::Crashed.is_hang_candidate());
        assert!(!TargetState::DutOff.is_hang_candidate());
        assert!(!TargetState::Disconnected.is_hang_candidate());
        assert!(TargetState::Dutabo.is_hang_candidate()); // Dutabo: keep watchdog alive
    }

    #[test]
    fn test_atomic_write() {
        let (sm, tmp) = create_test_state_manager(60, 3);
        let state_file = tmp.path().join(".dut-serial/target-state");

        sm.atomic_write(&state_file, "test-state");
        assert_eq!(std::fs::read_to_string(&state_file).unwrap(), "test-state");

        sm.atomic_write(&state_file, "another-state");
        assert_eq!(
            std::fs::read_to_string(&state_file).unwrap(),
            "another-state"
        );
    }

    #[test]
    fn test_statusline_cache_write() {
        let (mut sm, tmp) = create_test_state_manager(60, 3);
        let cache_file = tmp.path().join(".dut-serial/statusline-cache");
        let shm_file = std::path::PathBuf::from(format!(
            "/dev/shm/claude-status-{}",
            StateManager::project_hash(tmp.path())
        ));

        sm.transition(TargetState::Active);
        assert!(cache_file.exists());
        let content = std::fs::read_to_string(&cache_file).unwrap();
        assert!(content.contains("serial:active"));
        // /dev/shm cache is only written if /dev/shm exists
        if std::path::Path::new("/dev/shm").is_dir() {
            assert!(shm_file.exists());
            let shm_content = std::fs::read_to_string(&shm_file).unwrap();
            assert!(shm_content.contains("serial:active"));
        }

        sm.transition(TargetState::DutOff);
        let content = std::fs::read_to_string(&cache_file).unwrap();
        assert!(content.contains("serial:DUT-off"));
    }

    #[test]
    fn test_format_statusline_all_states_have_text() {
        let (sm, _tmp) = create_test_state_manager(60, 3);
        for state in &[
            TargetState::Active,
            TargetState::Booting,
            TargetState::Booted,
            TargetState::UBoot,
            TargetState::Crashed,
            TargetState::Disconnected,
            TargetState::DutOff,
        ] {
            let text = sm.format_statusline(*state);
            assert!(!text.is_empty(), "{:?} should have statusline text", state);
        }
        // Stopped/Connecting produce empty string (no display)
        assert_eq!(sm.format_statusline(TargetState::Stopped), "");
        assert_eq!(sm.format_statusline(TargetState::Connecting), "");
    }

    #[test]
    fn test_same_state_no_transition() {
        let (mut sm, tmp) = create_test_state_manager(60, 3);
        let state_file = tmp.path().join(".dut-serial/target-state");

        sm.transition(TargetState::Active);
        let content1 = std::fs::read_to_string(&state_file).unwrap();

        // Transition to same state should not update file
        sm.transition(TargetState::Active);
        let content2 = std::fs::read_to_string(&state_file).unwrap();
        assert_eq!(content1, content2);
    }

    #[test]
    fn test_disconnected_state() {
        let (mut sm, _tmp) = create_test_state_manager(60, 3);

        sm.transition(TargetState::Disconnected);
        assert_eq!(sm.current(), TargetState::Disconnected);
        assert_eq!(sm.external_state(), Some(TargetState::Disconnected));
    }

    #[test]
    fn test_format_statusline_has_ansi_colors() {
        let sm = StateManager::new(&std::env::temp_dir(), 60, 3, ".dut-serial", "test-board");
        let text = sm.format_statusline(TargetState::Active);
        assert!(
            text.contains("\x1b[32m"),
            "Active state must have green ANSI code, got: {:?}",
            text
        );
        assert!(text.contains("\x1b[0m"), "Must have ANSI reset code");
        assert!(
            text.contains("test-board"),
            "Must use DUT alias 'test-board'"
        );

        let crash = sm.format_statusline(TargetState::Crashed);
        assert!(
            crash.contains("\x1b[31m"),
            "Crashed state must have red ANSI code"
        );
        assert!(crash.contains("\x1b[0m"), "Must have ANSI reset code");

        let dutoff = sm.format_statusline(TargetState::DutOff);
        assert!(
            dutoff.contains("\x1b[31m"),
            "DUT-off state must have red ANSI code"
        );
        assert!(dutoff.contains("\x1b[0m"), "Must have ANSI reset code");

        // "default" alias should use "serial" as label (backward compat)
        let sm_default = StateManager::new(&std::env::temp_dir(), 60, 3, ".dut-serial", "default");
        let text2 = sm_default.format_statusline(TargetState::Active);
        assert!(
            text2.contains("serial"),
            "Default alias must use 'serial' label"
        );

        // Empty alias should also use "serial"
        let sm_empty = StateManager::new(&std::env::temp_dir(), 60, 3, ".dut-serial", "");
        let text3 = sm_empty.format_statusline(TargetState::Active);
        assert!(
            text3.contains("serial"),
            "Empty alias must use 'serial' label"
        );
    }
}
