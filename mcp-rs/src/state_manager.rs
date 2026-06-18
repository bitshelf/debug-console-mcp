//! State manager — hysteresis debounce + atomic state file writes +
//! three-tier state space.
//!
//! Three tiers:
//! - Internal: stopped, connecting, active, booting, booted, uboot, crashed, DUT-off, disconnected
//! - MCP API: filtered for external visibility (excludes stopped/connecting)
//! - Statusline file: writes 7 external states; stopped → delete file; connecting → no write

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
        matches!(self, Self::Booting | Self::Active)
    }
}

pub struct StateManager {
    current: TargetState,
    state_file: PathBuf,
    /// Project-local cache (fallback, read by Python hook)
    cache_file: PathBuf,
    /// /dev/shm cache — zero-syscall read by Python hook via `cat`
    shm_cache_file: PathBuf,
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
}

impl StateManager {
    pub fn new(project_dir: &Path, hang_timeout: u64, hysteresis: u32, dut_dir: &str) -> Self {
        let dut_dir_path = project_dir.join(dut_dir);
        let state_file = dut_dir_path.join("target-state");
        let cache_file = dut_dir_path.join("statusline-cache");

        // /dev/shm cache: keyed by project path hash so multiple projects don't collide
        let shm_dir = if std::path::Path::new("/dev/shm").is_dir() {
            "/dev/shm"
        } else {
            "/tmp"
        };
        let project_hash = Self::project_hash(project_dir);
        let shm_cache_file = std::path::PathBuf::from(format!(
            "{}/claude-status-{}", shm_dir, project_hash
        ));

        std::fs::create_dir_all(&dut_dir_path).ok();

        Self {
            current: TargetState::Stopped,
            state_file,
            cache_file,
            shm_cache_file,
            hang_timeout_secs: hang_timeout,
            hysteresis,
            hang_count: 0,
            last_data_time: Instant::now(),
            heartbeat_pending: false,
            last_probe_time: Instant::now(),
            heartbeat_missed: 0,
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

    /// Write PID file — call only after lock is acquired.
    pub fn write_pid(&self, project_dir: &Path, dut_dir: &str) {
        let pid_file = project_dir.join(dut_dir).join("mcp.pid");
        std::fs::write(&pid_file, std::process::id().to_string()).ok();
    }

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

    pub fn transition(&mut self, new: TargetState) {
        if new == self.current {
            return;
        }
        tracing::info!("StateManager: {} → {}", self.current.as_str(), new.as_str());
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
            | TargetState::DutOff => {
                self.atomic_write(&self.state_file, new.as_str());
                let text = Self::format_statusline(new);
                self.atomic_write(&self.cache_file, &text);
                self.atomic_write(&self.shm_cache_file, &text);
            }
        }
    }

    /// ANSI-formatted statusline text for a given state.
    fn format_statusline(state: TargetState) -> String {
        let text: &str = match state {
            TargetState::Active => "\x1b[32m● serial:active\x1b[0m",
            TargetState::Booting => "\x1b[33m◐ serial:booting\x1b[0m",
            TargetState::Booted => "\x1b[32m● serial:booted\x1b[0m",
            TargetState::UBoot => "\x1b[36m● serial:uboot\x1b[0m",
            TargetState::Crashed => "\x1b[31m✗ serial:crashed\x1b[0m",
            TargetState::Disconnected => "\x1b[31m✗ serial:disconnected\x1b[0m",
            TargetState::DutOff => "\x1b[31m✗ serial:DUT-off\x1b[0m",
            TargetState::Stopped | TargetState::Connecting => "",
        };
        text.to_string()
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
                    self.heartbeat_missed, data_elapsed, probe_elapsed
                );
                if self.heartbeat_missed >= self.hysteresis {
                    tracing::warn!("Heartbeat: {} misses → DUT-off", self.heartbeat_missed);
                    self.transition(TargetState::DutOff);
                } else {
                    // Request another probe
                    self.heartbeat_pending = true;
                }
            } else if data_elapsed > self.hang_timeout_secs as f64 && probe_elapsed > self.hang_timeout_secs as f64 {
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

    fn atomic_write(&self, path: &Path, content: &str) {
        let tmp = path.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, content) {
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
        let sm = StateManager::new(tmp.path(), hang_timeout, hysteresis, ".dut-serial");
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
        let sm = StateManager::new(tmp.path(), 60, 3, ".dut-serial");
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
        assert!(TargetState::Active.is_hang_candidate());    // heartbeat probe
        assert!(TargetState::Booting.is_hang_candidate());
        assert!(!TargetState::Booted.is_hang_candidate());
        assert!(!TargetState::UBoot.is_hang_candidate());
        assert!(!TargetState::Crashed.is_hang_candidate());
        assert!(!TargetState::DutOff.is_hang_candidate());
        assert!(!TargetState::Disconnected.is_hang_candidate()); // network issue, not target hang
    }

    #[test]
    fn test_atomic_write() {
        let (sm, tmp) = create_test_state_manager(60, 3);
        let state_file = tmp.path().join(".dut-serial/target-state");

        sm.atomic_write(&state_file, "test-state");
        assert_eq!(std::fs::read_to_string(&state_file).unwrap(), "test-state");

        sm.atomic_write(&state_file, "another-state");
        assert_eq!(std::fs::read_to_string(&state_file).unwrap(), "another-state");
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
        for state in &[
            TargetState::Active,
            TargetState::Booting,
            TargetState::Booted,
            TargetState::UBoot,
            TargetState::Crashed,
            TargetState::Disconnected,
            TargetState::DutOff,
        ] {
            let text = StateManager::format_statusline(*state);
            assert!(!text.is_empty(), "{:?} should have statusline text", state);
        }
        // Stopped/Connecting produce empty string (no display)
        assert_eq!(StateManager::format_statusline(TargetState::Stopped), "");
        assert_eq!(StateManager::format_statusline(TargetState::Connecting), "");
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
}
