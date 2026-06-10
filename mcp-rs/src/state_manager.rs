//! State manager — 滞后防抖 + 原子写状态文件 + 三层状态空间。
//!
//! 三层:
//! - 内部: stopped, connecting, active, booting, booted, uboot, crashed, DUT-off, disconnected
//! - MCP API: 过滤后外露 (排除 stopped/connecting)
//! - statusline 文件: 写入 7 种外部状态; stopped → 保留文件; connecting → 不写

use std::path::{Path, PathBuf};
use std::time::Instant;

/// 目标板状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TargetState {
    Stopped,
    Connecting,
    Active,
    Booting,
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

    /// MCP API 可见的状态 (排除 stopped/connecting)
    pub fn is_external(&self) -> bool {
        !matches!(self, Self::Stopped | Self::Connecting)
    }

    /// 挂死/心跳检测候选: booting + active
    /// booting: 长时间无输出 → 判定 hang
    /// active:  长时间无输出 → 发送心跳探针检测是否存活
    fn is_hang_candidate(&self) -> bool {
        matches!(self, Self::Booting | Self::Active)
    }
}

pub struct StateManager {
    current: TargetState,
    state_file: PathBuf,
    hang_timeout_secs: u64,
    hysteresis: u32,
    hang_count: u32,
    last_data_time: Instant,
    /// 心跳探针: active 状态无数据时设为 true，触发 serial_engine 发探针
    pub heartbeat_pending: bool,
    /// 上次发探针的时间 (用于给板子响应窗口)
    last_probe_time: Instant,
    /// 探针无响应次数 (累计超过 hysteresis → DUT-off)
    heartbeat_missed: u32,
}

impl StateManager {
    pub fn new(project_dir: &Path, hang_timeout: u64, hysteresis: u32, dut_dir: &str) -> Self {
        let dut_dir_path = project_dir.join(dut_dir);
        let state_file = dut_dir_path.join("target-state");
        std::fs::create_dir_all(&dut_dir_path).ok();

        Self {
            current: TargetState::Stopped,
            state_file,
            hang_timeout_secs: hang_timeout,
            hysteresis,
            hang_count: 0,
            last_data_time: Instant::now(),
            heartbeat_pending: false,
            last_probe_time: Instant::now(),
            heartbeat_missed: 0,
        }
    }

    /// 写入 PID 文件 — 仅 lock 成功后调用
    pub fn write_pid(&self, project_dir: &Path, dut_dir: &str) {
        let pid_file = project_dir.join(dut_dir).join("mcp.pid");
        std::fs::write(&pid_file, std::process::id().to_string()).ok();
    }

    pub fn current(&self) -> TargetState {
        self.current
    }

    /// MCP API 返回的状态 (stopped/connecting → None)
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
                // MCP Server 关闭 → 删除状态文件，statusline 不显示任何状态
                self.delete_state_file();
                tracing::info!("StateManager: deleted state file (server stopped)");
            }
            TargetState::Connecting => {
                // 不写文件，避免 statusline 闪烁
            }
            _ => {
                self.atomic_write(new.as_str());
            }
        }
    }

    /// 每次收到串口数据时调用 — 重置挂死和心跳计数器
    pub fn on_activity(&mut self) {
        self.last_data_time = Instant::now();
        self.hang_count = 0;
        self.heartbeat_pending = false;
        self.heartbeat_missed = 0;
    }

    /// 标记心跳探针已发送 (由 serial_engine 调用), 记录发送时间
    pub fn mark_probe_sent(&mut self) {
        self.last_probe_time = Instant::now();
        self.heartbeat_pending = false;
    }

    /// 检测挂死 / 心跳超时
    /// booting: 长时间无输出 → hang_count++ → DUT-off
    /// active:  长时间无输出 → 发换行探针 (不执行命令);
    ///          探针后 5s 内无响应 → miss++ → DUT-off
    pub fn check_hang(&mut self) {
        if !self.current.is_hang_candidate() {
            self.hang_count = 0;
            self.heartbeat_pending = false;
            return;
        }
        let data_elapsed = self.last_data_time.elapsed().as_secs_f64();
        if self.current == TargetState::Active {
            let probe_elapsed = self.last_probe_time.elapsed().as_secs_f64();
            // 探针已发送且等待超过 5s 响应窗口
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
                    // 再次请求发探针
                    self.heartbeat_pending = true;
                }
            } else if data_elapsed > self.hang_timeout_secs as f64 && probe_elapsed > self.hang_timeout_secs as f64 {
                // 首次超时，请求发探针
                self.heartbeat_pending = true;
            }
        } else {
            // Booting 状态 — 无输出超时 → 判定为 hang
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

    fn atomic_write(&self, state: &str) {
        let tmp = self.state_file.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, state) {
            tracing::error!("StateManager: write failed: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &self.state_file) {
            tracing::error!("StateManager: rename failed: {e}");
        }
    }

    fn delete_state_file(&self) {
        if self.state_file.exists() {
            if let Err(e) = std::fs::remove_file(&self.state_file) {
                tracing::error!("StateManager: delete failed: {e}");
            }
        }
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

        sm.atomic_write("test-state");
        assert_eq!(std::fs::read_to_string(&state_file).unwrap(), "test-state");

        sm.atomic_write("another-state");
        assert_eq!(std::fs::read_to_string(&state_file).unwrap(), "another-state");
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
