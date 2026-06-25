//! SerialEngine — 核心引擎, 协调 console, log, detector, state, commands, relay。
//!
//! Lifespan: start → 获取锁 + 打开串口 + 启动读循环/看门狗
//!           stop  → 关闭一切 + 释放资源

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::boot_detector::{BootEvent, BootStageDetector};
use crate::reconnect::ReconnectManager;
use crate::command_queue::CommandQueue;
use crate::config::Config;
use crate::connection_learner::{ConnectionLearner, LearnConfig, LearnMethod, LearnResult};
use crate::console::SerialConsoleDriver;
use crate::lock_manager;
use crate::log_manager::LogManager;
use crate::power_control::Button;
use crate::relay_manager::RelayManager;
use crate::state_manager::{StateManager, TargetState};

pub struct SerialEngine {
    pub console: SerialConsoleDriver,
    pub detector: BootStageDetector,
    pub state: StateManager,
    pub logs: LogManager,
    pub commands: CommandQueue,
    pub relay: RelayManager,
    pub config: Config,
    running: bool,
    read_handle: Option<tokio::task::JoinHandle<()>>,
    watchdog_handle: Option<tokio::task::JoinHandle<()>>,
    host: String,
    relay_host: String,
    serial_target: String,
    login_user: String,
    login_pass: String,
    pub interrupt_char: u8,
    /// poll_logs 的 file position tracking
    poll_position: u64,
    /// When paused, the engine skips sending data (heartbeat, login, etc.)
    /// Used by dutabo serial to take over the serial port without Agent interference.
    paused: bool,
    /// Reconnection manager with exponential backoff.
    reconnect: ReconnectManager,
}

impl SerialEngine {
    pub fn new(config: Config) -> Self {
        let project_dir = config
            .project_dir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        let host = config.serial_ip();
        let relay_host = config.relay_ip();
        let serial_target = config.serial_target();
        let dut_dir = config.dut_dir();
        // 提取所有需要的配置值（在 config 被 move 之前）
        let login_user = config.login_user();
        let login_pass = config.login_pass();
        let login_prompt = config.login_prompt();
        let interrupt_char = config.uboot_interrupt_char();

        let mut detector = BootStageDetector::new();
        if !login_prompt.is_empty() {
            detector.set_login_regex(&login_prompt);
        }

        Self {
            console: SerialConsoleDriver::new(host.clone(), serial_target.clone()),
            detector,
            state: StateManager::new(
                &project_dir,
                config.hang_timeout(),
                config.hang_hysteresis(),
                &dut_dir,
                config.get("DUT_ALIAS"),
            ),
            logs: LogManager::new(
                &project_dir,
                config.max_archived_logs(),
                config.max_log_file_size_mb(),
                &dut_dir,
            ),
            commands: CommandQueue::new(),
            relay: RelayManager::new(
                relay_host.clone(),
                config.relay_port(),
                config.reset_channel(),
                config.maskrom_channel(),
                config.recovery_channel(),
            ),
            config,
            running: false,
            read_handle: None,
            watchdog_handle: None,
            host,
            relay_host,
            serial_target,
            login_user,
            login_pass,
            interrupt_char,
            poll_position: 0,
            paused: false,
            reconnect: ReconnectManager::new(),
        }
    }

    /// 启动引擎: 项目单例检查 → 获取串口锁 → 打开串口 → 启动后台任务
    #[tracing::instrument(skip(self), fields(host, target))]
    pub async fn start(&mut self) -> Result<(), String> {
        tracing::Span::current().record("host", &self.host);
        tracing::Span::current().record("target", &self.serial_target);
        let project_dir = self
            .config
            .project_dir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        let dut_dir = self.config.dut_dir();

        // 0. Validate config — catch common misconfigurations early
        {
            let mut issues: Vec<String> = Vec::new();
            if self.config.dev_host_ip().is_empty() {
                issues.push("dev_host.ip not set".into());
            }
            let port = self.config.serial_target();
            if port.is_empty() || port == "0" {
                issues.push("serial.port not set".into());
            }
            if self.config.login_user().is_empty() {
                tracing::warn!("[AGENT-NOTIFY] login_user not set — auto-login disabled");
            }
            let ref_log = self.config.reference_log();
            if !ref_log.is_empty() {
                let ref_path = std::path::PathBuf::from(&ref_log);
                let abs_path = if ref_path.is_relative() {
                    self.config.project_dir.as_ref()
                        .map(|p| p.join(&ref_path))
                        .unwrap_or(ref_path)
                } else {
                    ref_path
                };
                if !abs_path.exists() {
                    tracing::warn!(
                        "[AGENT-NOTIFY] reference_log '{}' not found. \
                         Run serial_reset(wait_boot=true) to auto-capture.",
                        abs_path.display()
                    );
                }
            }
            if !issues.is_empty() {
                for issue in &issues {
                    tracing::error!("[AGENT-NOTIFY] Config error: {issue}");
                }
                return Err(format!("Config errors: {}. Fix .target.toml.", issues.join("; ")));
            }
        }

        // Optional: verify udev alias on dev host (best-effort, doesn't block)
        let dut_alias = self.config.get_str_or("DUT_ALIAS", "default");
        if !dut_alias.is_empty() && dut_alias != "default" {
            let dev_ip = self.config.dev_host_ip();
            if !dev_ip.is_empty() {
                let alias_path = format!("/dev/serial/by-alias/{}", dut_alias);
                let check = std::process::Command::new("ssh")
                    .args(["-o", "ConnectTimeout=3", &format!("linaro@{}", dev_ip), "test", "-e", &alias_path])
                    .status();
                match check {
                    Ok(status) if status.success() => {
                        tracing::info!("udev alias verified: {}", alias_path);
                    }
                    _ => {
                        tracing::warn!(
                            "[AGENT-NOTIFY] udev alias '{}' not found on dev host. \
                             Run: ssh {} ls /dev/serial/by-alias/",
                            alias_path, dev_ip
                        );
                    }
                }
            }
        }

        // 1. 项目级单例: 同一 project_dir 只能有一个 MCP，自动替换旧进程
        if let Some(conflicting_pid) = lock_manager::check_project_singleton(&project_dir, &dut_dir)
        {
            tracing::warn!("Killing stale MCP process (PID {conflicting_pid})");
            unsafe { libc::kill(conflicting_pid as i32, libc::SIGTERM) };
            std::thread::sleep(std::time::Duration::from_millis(800));
            // Clean stale PID files (project-level + per-DUT + session)
            let pid_file = project_dir.join(&dut_dir).join("mcp.pid");
            std::fs::remove_file(&pid_file).ok();
            let auto_dut_alias = self.config.get_str_or("DUT_ALIAS", "default");
            let alias_pid = project_dir.join(&dut_dir).join(&auto_dut_alias).join("mcp.pid");
            std::fs::remove_file(&alias_pid).ok();
            let session_pid = project_dir.join(&dut_dir).join(".session-pid");
            std::fs::remove_file(&session_pid).ok();
        }

        // 2. 获取串口互斥锁
        let lock_dir = self.config.lock_dir();
        if let Some(conflicting_pid) =
            lock_manager::acquire_lock(&self.host, &self.serial_target, &lock_dir)
        {
            return Err(format!(
                "Target {}:{} is already in use by PID {}.",
                self.host, self.serial_target, conflicting_pid
            ));
        }

        // 3. 写入 PID 文件
        self.state.write_pid(&project_dir, &dut_dir);

        // 4. 设置 write_fn for CommandQueue
        let write_tx = self.console.write_sender();
        self.commands.set_write_fn(Box::new(move |data| {
            write_tx.try_send(data.to_vec()).ok();
        }));

        // 5. 确保日志文件已打开 (不切割 — 板子可能早已在运行)
        self.logs.ensure_current_file();

        // Auto-create per-DUT directory structure if missing
        let auto_dut_alias = self.config.get_str_or("DUT_ALIAS", "default");
        let alias_dir = project_dir.join(&dut_dir).join(&auto_dut_alias).join("logs");
        if let Err(e) = std::fs::create_dir_all(&alias_dir) {
            tracing::warn!("Cannot create DUT log dir {}: {e}", alias_dir.display());
        } else if !alias_dir.exists() {
            // create_dir_all reports Ok even if dir already exists; check it's there
            tracing::debug!("DUT log dir ready: {}", alias_dir.display());
        }

        // 6. 连接串口 + stty -echo + 探测初始状态
        match self.console.connect().await {
            Ok(()) => {
                tracing::info!("Serial connected to {}:{}", self.host, self.serial_target);
                // Disable echo before any command — avoids marker-in-echo garbling.
                self.console.sendline("stty -echo");
                tokio::time::sleep(Duration::from_millis(200)).await;
                self.probe_initial_state().await;
                // Warmup: prime the serial pipeline so the first real command
                // doesn't return empty (BusyBox/ser2net buffering issue).
                self.console.sendline("echo warmup");
                tokio::time::sleep(Duration::from_millis(150)).await;
                let _ = self.console.read_available(Duration::from_millis(100), 256).await;
            }
            Err(e) => {
                tracing::warn!("Cannot open serial: {e}");
                self.state.transition(TargetState::Disconnected);
            }
        }

        // 7. Auto-load reference boot log if configured (StageLearner adaptive mode)
        let ref_log = self.config.reference_log();
        if !ref_log.is_empty() {
            let ref_path = std::path::PathBuf::from(&ref_log);
            if ref_path.exists() {
                match self.detector.load_reference(&ref_path) {
                    Ok(()) => {
                        tracing::info!("Auto-loaded reference log: {}", ref_log);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load reference log {}: {e}", ref_log);
                    }
                }
            } else {
                tracing::warn!(
                    "[AGENT-NOTIFY] Reference boot log missing: {}. \
                     The StageLearner needs a reference boot log for accurate stage detection. \
                     Run: serial_reset(wait_boot=true) to auto-capture one.",
                    ref_log
                );
            }
        }

        // 8. 标记运行状态 (后台任务由 mcp/mcp_http spawn 并通过 set_background_tasks 注册)
        self.running = true;

        tracing::info!(
            "[{}:{}] SerialEngine started",
            self.host,
            self.serial_target
        );
        Ok(())
    }

    /// 夺取串口所有权（供 MCP tool serial_claim 调用）
    pub async fn claim_serial(&mut self) -> serde_json::Value {
        let lock_dir = self.config.lock_dir();
        // 释放并重新获取锁
        lock_manager::release_lock(&self.host, &self.serial_target, &lock_dir);
        if let Some(conflicting) =
            lock_manager::acquire_lock(&self.host, &self.serial_target, &lock_dir)
        {
            return serde_json::json!({
                "success": false,
                "error": format!("Lock still held by PID {conflicting}")
            });
        }
        // 写新的 mcp.pid
        let project_dir = self
            .config
            .project_dir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        self.state.write_pid(&project_dir, &self.config.dut_dir());
        // 重连串口
        match self.console.connect().await {
            Ok(()) => {
                self.probe_initial_state().await;
                self.running = true;
                serde_json::json!({"success": true, "message": "Serial claimed"})
            }
            Err(e) => {
                serde_json::json!({"success": false, "error": format!("{e}")})
            }
        }
    }

    /// 停止引擎
    #[tracing::instrument(skip(self))]
    pub async fn stop(&mut self) {
        self.running = false;
        if let Some(h) = self.read_handle.take() {
            h.abort();
        }
        if let Some(h) = self.watchdog_handle.take() {
            h.abort();
        }
        if self.console.is_open() {
            self.console.sendline("stty echo");
        }
        self.console.close();
        self.relay.close();
        self.logs.close();
        let lock_dir = self.config.lock_dir();
        lock_manager::release_lock(&self.host, &self.serial_target, &lock_dir);
        self.state.transition(TargetState::Stopped);
        tracing::info!(
            "[{}:{}] SerialEngine stopped",
            self.host,
            self.serial_target
        );
    }

    /// 保存后台任务 handle (由 mcp/mcp_http spawn 后调用)
    pub fn set_background_tasks(
        &mut self,
        read_handle: tokio::task::JoinHandle<()>,
        watchdog_handle: tokio::task::JoinHandle<()>,
    ) {
        self.read_handle = Some(read_handle);
        self.watchdog_handle = Some(watchdog_handle);
    }

    /// 检查事件列表中是否包含启动完成信号
    fn is_boot_complete(events: &[BootEvent]) -> bool {
        events.iter().any(|e| {
            matches!(e,
                BootEvent::Stage(s) if matches!(s.as_str(),
                    "shell" | "android_shell" | "android_adbd"
                    | "android_bootanim" | "android_surfaceflinger"
                    | "android_boot_completed"
                )
            )
        })
    }

    /// 启动后探测串口当前状态
    #[tracing::instrument(skip(self), fields(result))]
    async fn probe_initial_state(&mut self) {
        let _start_state = self.state.current();
        match self
            .console
            .read_available(Duration::from_secs(1), 4096)
            .await
        {
            Ok(data) if !data.is_empty() => {
                let filtered = strip_ser2net_banner(&data);
                let has_real_data = !filtered.is_empty();
                let probe_data = if has_real_data { &filtered } else { &data };

                if has_real_data {
                    self.logs.write(probe_data);
                    let events = self.detector.feed(probe_data);
                    if Self::is_boot_complete(&events) {
                        self.state.transition(TargetState::Active);
                        tracing::info!("Probe: boot complete → active");
                        return;
                    }
                    if events.iter().any(|e| matches!(e, BootEvent::BootStart)) {
                        self.state.transition(TargetState::Booting);
                        tracing::info!("Probe: SPL → booting");
                        return;
                    }
                    if events
                        .iter()
                        .any(|e| matches!(e, BootEvent::Stage(s) if s == "uboot"))
                    {
                        self.state.transition(TargetState::UBoot);
                        tracing::info!("Probe: at U-Boot prompt");
                        return;
                    }
                }

                self.console.sendline("");
                self.console.drain_writes().await;
                tokio::time::sleep(Duration::from_millis(500)).await;

                if let Ok(d2) = self
                    .console
                    .read_available(Duration::from_millis(800), 4096)
                    .await
                {
                    if !d2.is_empty() {
                        self.logs.write(&d2);
                        let e2 = self.detector.feed(&d2);
                        if Self::is_boot_complete(&e2) {
                            self.state.transition(TargetState::Active);
                            tracing::info!("Probe: 2nd pass boot complete → active");
                        } else if e2
                            .iter()
                            .any(|e| matches!(e, BootEvent::Stage(s) if s == "uboot"))
                        {
                            self.state.transition(TargetState::UBoot);
                            tracing::info!("Probe: at U-Boot prompt");
                        } else {
                            self.state.transition(TargetState::Active);
                            tracing::info!("Probe: responding → active");
                        }
                    } else {
                        self.state.transition(TargetState::DutOff);
                        tracing::warn!("Probe: not responding → DUT-off");
                    }
                } else {
                    self.state.transition(TargetState::Active);
                    tracing::info!("Probe: read timeout → active");
                }
            }
            Ok(_) => {
                self.state.transition(TargetState::Active);
                tracing::info!("Probe: no data → active");
            }
            Err(e) => {
                self.state.transition(TargetState::Disconnected);
                tracing::warn!("Probe: read error ({e}) → disconnected");
            }
        }
        // Note: each probe path that transitions to Active already calls
        // flush_boot_log + mark_boot_start internally. Do NOT call again here.
        tracing::Span::current().record("result", &tracing::field::display(
            self.state.external_state().map(|s| s.as_str()).unwrap_or("unknown")
        ));
    }

    /// Check if serial data contains a shutdown message → transition to DUT-off.
    fn maybe_detect_shutdown(&mut self, data: &[u8]) {
        let text = String::from_utf8_lossy(data);
        if text.contains("Power down")
            || text.contains("System halted")
            || text.contains("reboot: Power down")
        {
            self.state.transition(TargetState::DutOff);
            tracing::info!("Shutdown detected → DUT-off");
        }
    }

    /// 事件驱动读循环: 等待数据 (epoll/kqueue, 无轮询), 处理 watchdog
    pub async fn read_loop_iter(&mut self) {
        if !self.running {
            return;
        }
        // Always drain pending writes
        self.console.drain_writes().await;

        // Periodic statusline cache refresh (every 5s) — keeps timestamps fresh

        // Handle resume from dutabo: reconnect with backoff, then go active
        if !self.paused && self.state.current() == TargetState::Dutabo {
            let delay = self.reconnect.next_delay();
            tracing::info!(
                "[AGENT-NOTIFY] Dutabo resume, reconnecting in {:.1}s (backoff {:.0})",
                delay.as_secs_f64(),
                self.reconnect.current_backoff()
            );
            tokio::time::sleep(delay).await;
            if let Ok(()) = self.console.connect().await {
                self.console.sendline("stty -echo");
                tokio::time::sleep(Duration::from_millis(500)).await;
                self.probe_initial_state().await;
                self.reconnect.reset();
                tracing::info!("Reconnected after dutabo session");
            }
            return;
        }

        if self.paused {
            return;
        }

        // Check for dutabo takeover sentinel file
        if let Some(ref proj) = self.config.project_dir {
            let sentinel = proj.join(".dut-serial").join(".dutabo-active");
            if sentinel.exists() {
                if self.console.is_open() {
                    self.console.close(); // release serial so dutabo can connect
                    self.state.transition(crate::state_manager::TargetState::Dutabo);
                }
                return; // don't reconnect while dutabo is active
            }
        }
        self.console.drain_writes().await;
        // disconnected 状态 → 主动尝试重连 (exponential backoff)
        if self.state.current() == TargetState::Disconnected {
            let delay = self.reconnect.next_delay();
            tracing::info!(
                "[AGENT-NOTIFY] Serial disconnected, reconnecting in {:.1}s (backoff {:.0})",
                delay.as_secs_f64(),
                self.reconnect.current_backoff()
            );
            tokio::time::sleep(delay).await;
            if let Ok(()) = self.console.connect().await {
                self.console.sendline("stty -echo");
                tokio::time::sleep(Duration::from_millis(500)).await;
                self.probe_initial_state().await;
                self.reconnect.reset();
                tracing::info!("Auto-reconnected after backoff");
            } else {
                return; // next iteration will try with increased backoff
            }
        }
        // 事件驱动等待数据 (100ms 超时, 快速释放锁给 HTTP)
        if !self.console.wait_readable(Duration::from_millis(100)).await {
            return;
        }
        // 数据就绪, 读取处理
        match self
            .console
            .read_available(Duration::from_millis(0), 4096)
            .await
        {
            Ok(data) if !data.is_empty() => {
                self.state.on_activity();
                self.maybe_detect_shutdown(&data);
                if self.state.current() == TargetState::DutOff {
                    self.state.transition(TargetState::Active);
                    tracing::info!("Device resumed from DUT-off → active");
                }
                let clean_data = strip_ser2net_banner(&data);
                if !clean_data.is_empty() {
                    self.logs.write(&clean_data);
                }
                // Catch panics in detector.feed() — a malformed regex or unexpected
                // input should not crash the MCP server.
                let events = {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.detector.feed(&clean_data)
                    }));
                    match result {
                        Ok(events) => events,
                        Err(e) => {
                            let msg = if let Some(s) = e.downcast_ref::<String>() {
                                s.clone()
                            } else if let Some(s) = e.downcast_ref::<&str>() {
                                s.to_string()
                            } else {
                                "unknown panic".to_string()
                            };
                            tracing::error!("[AGENT-NOTIFY] SerialEngine panicked in detector.feed(): {}. Reconnecting...", msg);
                            self.state.transition(TargetState::Disconnected);
                            return;
                        }
                    }
                };
                self.handle_boot_events(events).await;
                let clean = strip_android_klog(&data);
                // Catch panics in feed_serial_data() — marker parsing or buffer
                // management bugs should not crash the MCP server.
                {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.commands.feed_serial_data(&clean);
                    }));
                    if let Err(e) = result {
                        let msg = if let Some(s) = e.downcast_ref::<String>() {
                            s.clone()
                        } else if let Some(s) = e.downcast_ref::<&str>() {
                            s.to_string()
                        } else {
                            "unknown panic".to_string()
                        };
                        tracing::error!("[AGENT-NOTIFY] SerialEngine panicked in commands.feed_serial_data(): {}. Reconnecting...", msg);
                        self.state.transition(TargetState::Disconnected);
                    }
                }
            }
            Ok(_) => {
                self.commands.check_timeouts();
            }
            Err(e) => {
                self.handle_read_error(e).await;
            }
        }

        // Drain command metrics from CommandQueue → StateManager
        let completed = self.commands.completed_count;
        let errors = self.commands.error_count;
        for _ in 0..completed {
            self.state.inc_command();
        }
        for _ in 0..errors {
            self.state.inc_error();
        }
        self.commands.completed_count = 0;
        self.commands.error_count = 0;
    }

    async fn handle_read_error(&mut self, e: std::io::Error) {
        tracing::warn!("Serial read error: {e}");
        let cur = self.state.current();
        // 连接断开 → disconnected (ser2net 挂了, 板子可能正常)
        // hang 由 watchdog 的 check_hang 检测, 不在此判断
        self.state.transition(TargetState::Disconnected);
        // Reconnection is deferred to read_loop_iter() — sole reconnection path
        // avoids duplicate probe race when read loop also tries to reconnect.
        tracing::info!("Disconnected (was {:?}), read_loop_iter will reconnect", cur);
    }

    /// 看门狗迭代 (由独立 spawn task 定期调用)
    pub fn watchdog_once(&mut self) {
        if !self.running || self.paused {
            return;
        }
        self.state.check_hang();
        let cur = self.state.current();
        // 心跳探针: active 状态长时间无数据
        if self.state.heartbeat_pending && cur == TargetState::Active {
            if self.console.is_open() {
                tracing::info!("Heartbeat probe");
                self.console.sendline("");
                self.state.mark_probe_sent();
            }
        }
        // 每 1s flush 检测器缓冲 (无 \n 的 prompt)
        if self.state.last_data_elapsed().as_secs() >= 1 {
            let events = self.detector.flush_line_buf();
            for event in events {
                if let BootEvent::Stage(ref s) = event {
                    if matches!(
                        s.as_str(),
                        "shell"
                            | "android_shell"
                            | "android_adbd"
                            | "android_bootanim"
                            | "android_surfaceflinger"
                            | "android_boot_completed"
                    ) {
                        self.state.transition(TargetState::Active);
                    }
                }
            }
        }
        if cur == TargetState::Booting && self.state.last_data_elapsed().as_secs() >= 2 {
            if self.console.is_open() {
                self.console.sendline("");
            }
        }
        // 文本相似度检测: 数据像参考日志 → 立即切 booting/uboot
        if let Some(ref learner) = self.detector.learner {
            if self.logs.ring_buffer.len() > 512 {
                let recent =
                    &self.logs.ring_buffer[self.logs.ring_buffer.len().saturating_sub(2048)..];
                let text = String::from_utf8_lossy(recent);
                if learner.is_boot_like(&text, 0.25) {
                    if cur == TargetState::Active {
                        // 文本相似度检测到重启 → 完整日志分割
                        self.logs.flush_boot_log();
                        self.logs.mark_boot_start();
                        self.detector.reset_cycle();
                        self.state.transition(TargetState::Booting);
                        tracing::info!("Text similarity: reboot detected → log rotated + booting");
                    }
                }
            }
        }
    }

    /// 处理启动检测事件
    async fn handle_boot_events(&mut self, events: Vec<BootEvent>) {
        for event in events {
            match event {
                BootEvent::BootStart => {
                    // DDR/SPL 出现 = 板子重启 = 新 boot cycle
                    // 无论当前状态如何，都分割日志（用 boot_detected 去重同 cycle）
                    let cur = self.state.current();
                    self.logs.flush_boot_log();
                    self.logs.mark_boot_start();
                    self.logs.truncate_unclassified();
                    tracing::info!("BootStart: new boot cycle (was {:?})", cur);
                    self.state.transition(TargetState::Booting);
                    self.detector.reset_cycle();
                }
                BootEvent::Autoboot => {
                    // 默认不中断启动，让板子正常 boot。
                    // 仅在 serial_enter_uboot 中明确要求时才发 Ctrl-C 进入 U-Boot。
                    tracing::debug!("Autoboot detected — letting board boot normally");
                }
                BootEvent::LoginPrompt => {
                    if !self.login_user.is_empty() {
                        tracing::info!(
                            "[AGENT-NOTIFY] Login prompt detected — auto-login as '{}'",
                            self.login_user
                        );
                        self.console.sendline(&self.login_user);
                    } else {
                        tracing::warn!("[AGENT-NOTIFY] Login prompt but no login_user set");
                    }
                }
                BootEvent::PasswordPrompt => {
                    if !self.login_pass.is_empty() {
                        tracing::info!("[AGENT-NOTIFY] Password prompt — sending password");
                        self.console.sendline(&self.login_pass);
                    }
                }
                BootEvent::Crash(crash_type, line) => {
                    self.state.transition(TargetState::Crashed);
                    tracing::warn!("CRASH [{crash_type}]: {line}");
                }
                BootEvent::Unclassified(lines) => {
                    // 追加未分类行到 unclassified.log（供 Agent 自学习分析）
                    if !lines.is_empty() {
                        if let Err(e) = self.logs.append_unclassified(&lines) {
                            tracing::warn!("Failed to write unclassified lines: {e}");
                        }
                    }
                }
                BootEvent::Stage(stage) => {
                    let new_state = match stage.as_str() {
                        "uboot" | "autoboot" => Some(TargetState::UBoot),
                        "shell"
                        | "android_shell"
                        | "android_adbd"
                        | "android_bootanim"
                        | "android_surfaceflinger"
                        | "android_boot_completed" => Some(TargetState::Active),
                        _ => Some(TargetState::Booting),
                    };
                    if let Some(ns) = new_state {
                        // 看到 shell prompt → 立即切 active, 不等待
                        if ns == TargetState::Active {
                            self.state.transition(TargetState::Active);
                        } else {
                            let cur = self.state.current();
                            if cur != ns && cur != TargetState::Crashed {
                                self.state.transition(ns);
                            }
                        }
                    }
                }
            }
        }
    }

    // ── MCP Tool 接口 ──

    /// 提交命令并返回 receiver — 调用方必须在释放 engine lock 后 await
    #[tracing::instrument(skip(self), fields(cmd = %command, timeout))]
    pub fn queue_command(
        &mut self,
        command: &str,
        timeout: f64,
    ) -> tokio::sync::oneshot::Receiver<crate::command_queue::CommandResult> {
        self.commands.execute(command.to_string(), timeout)
    }

    /// 在 U-Boot 提示符下发送原始命令 (即发即返回, 不阻塞)
    pub async fn send_uboot_command(&mut self, command: &str, _timeout: f64) -> serde_json::Value {
        self.console.sendline(command);
        // 短暂等待命令回显 (最多 1s), 不阻塞 read loop
        let mut output = String::new();
        if let Ok(data) = self
            .console
            .read_available(std::time::Duration::from_millis(500), 1024)
            .await
        {
            if !data.is_empty() {
                let clean = strip_ser2net_banner(&data);
                if !clean.is_empty() {
                    self.logs.write(&clean);
                }
                self.state.on_activity();
                let events = self.detector.feed(&clean);
                self.handle_boot_events(events).await;
                output = String::from_utf8_lossy(&clean).to_string();
            }
        }
        serde_json::json!({"sent": command, "output": output.trim()})
    }

    pub fn get_state_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state.external_state().map(|s| s.as_str()).unwrap_or(""),
            "boot_number": self.logs.boot_number(),
            "last_data_seconds": self.state.last_data_elapsed().as_secs_f64(),
            "log_path": self.logs.current_path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            "relay_configured": self.relay.configured(),
            "login_configured": !self.login_user.is_empty(),
            "uptime_secs": self.state.uptime_secs(),
            "command_count": self.state.command_count(),
            "error_count": self.state.error_count(),
            "pending_commands": self.commands.pending_len(),
        })
    }

    pub fn read_log(
        &self,
        archive_index: usize,
        lines: usize,
        pattern: Option<&str>,
    ) -> serde_json::Value {
        let result = self.logs.read_log(archive_index, lines, pattern);
        serde_json::json!({
            "content": result.content,
            "filename": result.filename,
            "total_lines": result.total_lines,
            "filtered_lines": result.filtered_lines,
        })
    }

    pub fn list_logs(&self) -> serde_json::Value {
        let archives = self.logs.list_archives();
        let current = self
            .logs
            .current_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        serde_json::json!({
            "archives": archives.iter().map(|a| serde_json::json!({
                "index": a.index,
                "filename": a.filename,
                "size_bytes": a.size_bytes,
            })).collect::<Vec<_>>(),
            "current": current,
        })
    }

    pub async fn reset_target(
        &mut self,
        wait_boot: bool,
        failure_retry: usize,
        failure_retry_interval: f64,
    ) -> serde_json::Value {
        let mut backend = self.build_power_control();
        if !backend.has_button(Button::Reset) {
            return serde_json::json!({"success": false, "error": "No reset control configured"});
        }
        // Immediate state transition (don't wait for relay).
        self.state.transition(TargetState::Booting);
        if let Err(e) = backend.pulse(Button::Reset, 500).await {
            return serde_json::json!({
                "success": false,
                "error": format!("reset control failed via {}: {e}", backend.name()),
                "new_boot_number": self.logs.boot_number(),
            });
        }
        self.logs.flush_boot_log();
        self.logs.mark_boot_start();
        self.detector.reset_cycle();

        if !wait_boot {
            return serde_json::json!({
                "success": true,
                "new_boot_number": self.logs.boot_number(),
                "log_path": self.logs.current_path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            });
        }

        // Wait for login: with force_prompt_wait (lava semantics). Retry the
        // whole reset+wait up to `failure_retry` times on timeout.
        let mut attempts = 0usize;
        loop {
            attempts += 1;
            let result = self.wait_pattern_internal_opts("login:", 120.0, true).await;
            if result["matched"].as_bool().unwrap_or(false) {
                return serde_json::json!({
                    "success": true,
                    "new_boot_number": self.logs.boot_number(),
                    "log_path": self.logs.current_path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
                    "boot_complete": true,
                    "attempts": attempts,
                });
            }
            if attempts >= failure_retry {
                return serde_json::json!({
                    "success": true,
                    "new_boot_number": self.logs.boot_number(),
                    "log_path": self.logs.current_path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
                    "boot_complete": false,
                    "attempts": attempts,
                    "error": "login prompt not detected within timeout",
                });
            }
            // Retry: re-assert relay reset and re-arm.
            tracing::info!("reset_target retry {}/{}", attempts, failure_retry);
            tokio::time::sleep(Duration::from_secs_f64(failure_retry_interval)).await;
            self.state.transition(TargetState::Booting);
            let mut backend = self.build_power_control();
            if backend.pulse(Button::Reset, 500).await.is_ok() {
                self.logs.flush_boot_log();
                self.logs.mark_boot_start();
                self.detector.reset_cycle();
            }
        }
    }

    /// Set up the U-Boot prompt watcher and return a receiver (caller must
    /// release the engine lock before awaiting).
    pub fn queue_enter_uboot(
        &mut self,
    ) -> tokio::sync::mpsc::UnboundedReceiver<crate::boot_detector::WatcherMatch> {
        let pattern = r"=>|U-Boot[>#]".to_string();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.detector.add_watcher(&pattern, tx);
        rx
    }

    /// Set up a single-pattern wait and return a receiver (caller must
    /// release the engine lock before awaiting). Kept for tools that need
    /// the lock-released await pattern (e.g. `serial_enter_uboot`).
    #[allow(dead_code)]
    pub fn queue_wait_pattern(
        &mut self,
        pattern: &str,
    ) -> tokio::sync::mpsc::UnboundedReceiver<crate::boot_detector::WatcherMatch> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.detector.add_watcher(pattern, tx);
        rx
    }

    /// Relay reset + initial Ctrl-C burst (holds the lock, ~1 second).
    ///
    /// Only does the relay reset + a short pre-burst. The long continuous
    /// flood is done by `continuous_flood` which releases the lock between
    /// bursts so the read loop can process U-Boot banners and trigger
    /// watchers.
    ///
    /// The key insight for bootdelay=0: SPL/BL31/OP-TEE don't read the
    /// serial port, so Ctrl-C chars just sit in the UART FIFO. When
    /// U-Boot's `abortboot()` finally calls `tstc()`, even ONE pending
    /// Ctrl-C is enough to interrupt. We send 1 byte per 100ms (not
    /// 100-byte floods) to avoid overflowing the 16-byte UART FIFO.
    pub async fn do_relay_reset_and_flood(&mut self) -> Result<(), String> {
        self.state.transition(TargetState::Booting);
        let ch = self.interrupt_char;
        let mut backend = self.build_power_control();
        if !backend.has_button(Button::Reset) {
            return Err("No reset control configured".into());
        }

        // Pre-reset burst: clear any pending input on the host side.
        if self.console.is_open() {
            let pre: Vec<u8> = vec![ch; 4];
            self.console.write_raw(&pre).await;
        }

        // Reset via the configured backend — board begins rebooting.
        backend
            .pulse(Button::Reset, 500)
            .await
            .map_err(|e| format!("reset control failed via {}: {e}", backend.name()))?;
        self.logs.flush_boot_log();
        self.logs.mark_boot_start();
        self.detector.reset_cycle();

        // Short post-reset burst (the long flood is done separately).
        for _ in 0..5 {
            if self.console.is_open() {
                self.console.write_raw(&[ch]).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    /// Send one Ctrl-C byte (called in a loop by the enter-uboot tool,
    /// with the lock released between calls so the read loop can process
    /// U-Boot banners and trigger watchers).
    pub async fn flood_one(&mut self) {
        if self.console.is_open() {
            self.console.write_raw(&[self.interrupt_char]).await;
        }
    }

    /// Force the target into Rockchip MASKROM mode via the relay sequence.
    pub async fn enter_maskrom(&mut self) -> serde_json::Value {
        let mut backend = self.build_power_control();
        if !backend.has_button(Button::Reset) {
            return serde_json::json!({"success": false, "error": "No reset control configured"});
        }
        if !backend.has_button(Button::Maskrom) {
            return serde_json::json!({"success": false, "error": "MASKROM control not configured"});
        }
        let result = async {
            backend.press(Button::Maskrom).await?;
            tokio::time::sleep(Duration::from_millis(100)).await;
            backend.pulse(Button::Reset, 500).await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            backend.release(Button::Maskrom).await
        }
        .await;
        if let Err(e) = result {
            let _ = backend.release(Button::Maskrom).await;
            return serde_json::json!({
                "success": false,
                "error": format!("MASKROM control failed via {}: {e}", backend.name()),
            });
        }
        self.state.transition(TargetState::Booting);
        self.logs.flush_boot_log();
        self.logs.mark_boot_start();
        self.detector.reset_cycle();
        serde_json::json!({
            "success": true,
            "state_after": self.state.current().as_str(),
        })
    }

    /// Wait for a pattern (no probe-on-timeout). Convenience wrapper around
    /// `wait_pattern_internal_opts` for callers that don't need the probe
    /// behavior.
    #[allow(dead_code)]
    pub async fn wait_pattern_internal(
        &mut self,
        pattern: &str,
        timeout: f64,
    ) -> serde_json::Value {
        self.wait_pattern_internal_opts(pattern, timeout, false)
            .await
    }

    /// Wait for a pattern with optional `force_prompt_wait` semantics
    /// (labgrid/lava: on timeout, send a newline to provoke a prompt and
    /// retry up to `max_probes` times at `timeout/10` each).
    ///
    /// When `probe_on_timeout` is true and the wait times out, this sends
    /// `"\n"` to the console and retries — useful for noisy consoles where
    /// kernel logs overlap the prompt. Mirrors lava's `force_prompt_wait`
    /// (shell.py:332) 6× `timeout/10` cadence.
    pub async fn wait_pattern_internal_opts(
        &mut self,
        pattern: &str,
        timeout: f64,
        probe_on_timeout: bool,
    ) -> serde_json::Value {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        self.detector.add_watcher(pattern, tx.clone());

        let max_probes = if probe_on_timeout { 6usize } else { 0 };
        let partial = if probe_on_timeout {
            timeout / 10.0
        } else {
            timeout
        };
        let mut elapsed = 0.0f64;
        let mut probes = 0usize;

        loop {
            let remaining = (timeout - elapsed).max(0.0);
            let wait = partial.min(remaining);
            let result = tokio::time::timeout(Duration::from_secs_f64(wait), rx.recv()).await;

            match result {
                Ok(Some(m)) => {
                    self.detector.remove_watcher_group(&tx);
                    return serde_json::json!({
                        "matched": true,
                        "matched_line": m.line,
                        "pattern_index": m.pattern_index,
                    });
                }
                Ok(None) => {
                    self.detector.remove_watcher_group(&tx);
                    return serde_json::json!({
                        "matched": false,
                        "matched_line": null,
                    });
                }
                Err(_) => {
                    elapsed += wait;
                    if elapsed >= timeout || probes >= max_probes {
                        self.detector.remove_watcher_group(&tx);
                        tracing::warn!(
                            "Pattern '{}' not detected within {:.0}s. Target state: {:?}. Check serial_get_state.",
                            pattern, timeout, self.state.current()
                        );
                        return serde_json::json!({
                            "matched": false,
                            "matched_line": null,
                            "probes_sent": probes,
                        });
                    }
                    // Provoke a fresh prompt (lava force_prompt_wait).
                    if self.console.is_open() {
                        tracing::info!(
                            "wait_pattern timeout {:.1}s, probing with newline (attempt {}/{})",
                            elapsed,
                            probes + 1,
                            max_probes
                        );
                        self.console.sendline("");
                        self.console.drain_writes().await;
                    }
                    probes += 1;
                }
            }
        }
    }

    pub fn rotate_log(&mut self) -> serde_json::Value {
        self.logs.flush_boot_log();
        self.logs.mark_boot_start();
        self.detector.reset_cycle();
        serde_json::json!({
            "success": true,
            "filename": self.logs.current_path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        })
    }

    /// 增量获取新输出 (基于文件位置)
    pub fn poll_logs(&mut self) -> serde_json::Value {
        let path = match self.logs.current_path() {
            Some(p) => p.to_path_buf(),
            None => {
                return serde_json::json!({"lines": [], "since": 0});
            }
        };

        match std::fs::metadata(&path) {
            Ok(meta) => {
                let size = meta.len();
                if size <= self.poll_position {
                    return serde_json::json!({"lines": [], "since": self.poll_position});
                }
                match std::fs::File::open(&path) {
                    Ok(mut file) => {
                        use std::io::{Read, Seek, SeekFrom};
                        file.seek(SeekFrom::Start(self.poll_position)).ok();
                        let bytes_to_read = (size - self.poll_position) as usize;
                        let mut buf = vec![0u8; bytes_to_read];
                        let n = file.read(&mut buf).unwrap_or(0);
                        buf.truncate(n);
                        self.poll_position = size;
                        let content = String::from_utf8_lossy(&buf);
                        let lines: Vec<&str> = content.lines().collect();
                        serde_json::json!({
                            "lines": lines,
                            "since": self.poll_position,
                        })
                    }
                    Err(_) => serde_json::json!({"lines": [], "since": self.poll_position}),
                }
            }
            Err(_) => serde_json::json!({"lines": [], "since": self.poll_position}),
        }
    }

    /// Get unclassified lines collected by StageLearner (for Agent auto-learning).
    pub fn get_unclassified(&mut self) -> serde_json::Value {
        // Drain in-memory buffer first, then also read from file
        let mut all_lines: Vec<String> = self.detector.unclassified_lines.drain(..).collect();
        let file_lines = self.logs.read_unclassified();
        all_lines.extend(file_lines);
        let count = all_lines.len();
        let log_path =
            self.config.dut_dir().trim_end_matches('/').to_string() + "/unclassified.log";
        if count == 0 {
            return serde_json::json!({"lines": [], "count": 0, "log_path": log_path, "message": "No unclassified lines. StageLearner is matching all boot output, or no reference log is loaded."});
        }
        serde_json::json!({
            "lines": all_lines,
            "count": count,
            "log_path": log_path,
        })
    }

    /// Append key anchor lines to reference boot log and hot-reload StageLearner.
    pub fn append_reference(&mut self, lines: &str) -> serde_json::Value {
        let ref_path_str = self.config.reference_log();
        if ref_path_str.is_empty() {
            return serde_json::json!({"success": false, "error": "No reference_log configured. Add `reference_log` to .target.toml."});
        }
        let ref_path = std::path::PathBuf::from(&ref_path_str);
        // Ensure parent dir exists
        if let Some(parent) = ref_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // Append lines to reference log
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&ref_path)
        {
            Ok(mut file) => {
                use std::io::Write;
                for line in lines.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        writeln!(file, "{trimmed}").ok();
                    }
                }
            }
            Err(e) => {
                return serde_json::json!({"success": false, "error": format!("Cannot open reference log: {e}")});
            }
        }
        // Hot-reload StageLearner
        match self.detector.load_reference(&ref_path) {
            Ok(()) => {
                let fp_count = self
                    .detector
                    .learner
                    .as_ref()
                    .map(|l| l.fingerprints.len())
                    .unwrap_or(0);
                serde_json::json!({
                    "success": true,
                    "message": format!("Appended lines, reloaded reference log ({fp_count} fingerprints)"),
                    "fingerprints": fp_count,
                    "reference_log_path": ref_path_str,
                })
            }
            Err(e) => {
                // Append succeeded but reload failed — still report partial success
                serde_json::json!({
                    "success": true,
                    "warning": format!("Lines appended but reload failed: {e}"),
                    "reference_log_path": ref_path_str,
                })
            }
        }
    }

    pub fn get_config(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (k, v) in &self.config.values {
            map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        serde_json::Value::Object(map)
    }

    // ── Connection Learning ──────────────────────────────────────────────

    /// Run the hardware reset connection learning process.
    ///
    /// Requires relay configured. Executes 3 reset→capture cycles, compares
    /// first 50 lines similarity. If ≥93%, generates reference_log.
    /// If <10%, relay is marked broken.
    pub async fn learn_connection_hardware(&mut self) -> serde_json::Value {
        let mut power_ctrl = self.build_power_control();
        if !power_ctrl.has_button(Button::Reset) {
            return serde_json::json!({
                "success": false,
                "error": "No reset control configured. Cannot perform hardware reset learning.",
                "hint": "Configure reset channel or dev_ctl in .target.toml, or use software reboot learning."
            });
        }

        let dut_dir = self.config.dut_dir();
        let learn_dir = std::path::PathBuf::from(&dut_dir).join("learn");
        let ref_log = self.config.reference_log();
        let reference_log_path = if ref_log.is_empty() {
            std::path::PathBuf::from(&dut_dir).join("reference-boot.log")
        } else {
            std::path::PathBuf::from(&ref_log)
        };

        let mut learn_cfg = LearnConfig::default();
        learn_cfg.learn_dir = learn_dir;
        learn_cfg.reference_log = reference_log_path;

        let learner = ConnectionLearner::new(learn_cfg);

        // Verify relay (non-fatal — relay may work even if verify fails)
        if let Err(e) = power_ctrl.verify().await {
            tracing::warn!("Relay verify failed: {e} — continuing anyway");
        }

        let result = self.run_hardware_learning(&learner).await;
        Self::format_learn_result(&result)
    }

    /// Run the software reboot connection learning process (fallback).
    ///
    /// Two modes:
    /// 1. No relay, no existing reference_log → 3 reboots, compare against each other
    /// 2. No relay, HAS existing reference_log → 3 reboots, compare each against existing ref
    pub async fn learn_connection_software(&mut self) -> serde_json::Value {
        let dut_dir = self.config.dut_dir();
        let learn_dir = std::path::PathBuf::from(&dut_dir).join("learn");
        let ref_log = self.config.reference_log();
        let reference_log_path = if ref_log.is_empty() {
            std::path::PathBuf::from(&dut_dir).join("reference-boot.log")
        } else {
            std::path::PathBuf::from(&ref_log)
        };

        // Check if an existing reference log exists for comparison
        let existing_ref = if reference_log_path.exists() {
            std::fs::read_to_string(&reference_log_path).ok()
        } else {
            None
        };

        let mut learn_cfg = LearnConfig::default();
        learn_cfg.learn_dir = learn_dir;
        learn_cfg.reference_log = reference_log_path;

        let learner = ConnectionLearner::new(learn_cfg);

        let result = if existing_ref.is_some() {
            // Mode 2: compare each reboot cycle against existing reference log
            self.run_software_learning_with_ref(&learner, &existing_ref.unwrap())
                .await
        } else {
            // Mode 1: compare reboot cycles against each other
            self.run_software_learning(&learner).await
        };
        Self::format_learn_result(&result)
    }

    /// Verify the CH340 relay control: send ON, read back, send OFF, read back.
    pub async fn verify_relay(&mut self) -> serde_json::Value {
        let mut power_ctrl = self.build_power_control();
        if !power_ctrl.has_button(Button::Reset)
            && !power_ctrl.has_button(Button::Maskrom)
            && !power_ctrl.has_button(Button::Recovery)
        {
            return serde_json::json!({
                "success": false,
                "error": "No power control configured",
                "relay_configured": false,
            });
        }

        match power_ctrl.verify().await {
            Ok(true) => serde_json::json!({
                "success": true,
                "verified": true,
                "message": "Relay control verified — commands sent and read-back matched.",
                "backend": power_ctrl.name(),
            }),
            Ok(false) => serde_json::json!({
                "success": true,
                "verified": false,
                "message": "Relay responded but read-back verification is not supported by this backend.",
                "backend": power_ctrl.name(),
            }),
            Err(e) => serde_json::json!({
                "success": false,
                "verified": false,
                "error": format!("{e}"),
                "backend": power_ctrl.name(),
            }),
        }
    }

    /// Control a button (press/release/pulse) via the power control abstraction.
    pub async fn control_button(
        &mut self,
        button: &str,
        action: &str,
        delay_ms: Option<u64>,
    ) -> serde_json::Value {
        let btn = match button {
            "reset" => Button::Reset,
            "maskrom" => Button::Maskrom,
            "recovery" => Button::Recovery,
            _ => {
                return serde_json::json!({
                    "success": false,
                    "error": format!("Unknown button: {button}. Valid: reset, maskrom, recovery")
                });
            }
        };

        let mut backend = self.build_power_control();
        if !backend.has_button(btn) {
            return serde_json::json!({
                "success": false,
                "error": format!("Button '{button}' not configured. Add relay channel in .target.toml or configure dev_ctl."),
                "button": button,
            });
        }

        match action {
            "press" => match backend.press(btn).await {
                Ok(()) => {
                    serde_json::json!({"success": true, "button": button, "action": "press", "backend": backend.name()})
                }
                Err(e) => serde_json::json!({"success": false, "error": format!("{e}")}),
            },
            "release" => match backend.release(btn).await {
                Ok(()) => {
                    serde_json::json!({"success": true, "button": button, "action": "release", "backend": backend.name()})
                }
                Err(e) => serde_json::json!({"success": false, "error": format!("{e}")}),
            },
            "pulse" => {
                let ms = delay_ms.unwrap_or(500);
                match backend.pulse(btn, ms).await {
                    Ok(()) => {
                        serde_json::json!({"success": true, "button": button, "action": "pulse", "delay_ms": ms, "backend": backend.name()})
                    }
                    Err(e) => serde_json::json!({"success": false, "error": format!("{e}")}),
                }
            }
            _ => serde_json::json!({
                "success": false,
                "error": format!("Unknown action: {action}. Valid: press, release, pulse")
            }),
        }
    }

    // ── Internal learning helpers ───────────────────────────────────────

    /// Build a power control backend from the current config.
    ///
    /// If `dev_ctl` is configured in `.target.toml`, uses the external control
    /// program. Otherwise uses the CH340 relay backend.
    fn build_power_control(&self) -> crate::power_control::PowerControlBackend {
        let dev_ctl = self.config.dev_ctl();
        let channels = crate::power_control::ButtonChannels {
            reset: self.relay.reset_ch(),
            maskrom: self.relay.maskrom_ch(),
            recovery: self.relay.recovery_ch(),
        };
        if !dev_ctl.is_empty() {
            let dut_alias = self.config.get_str_or("DUT_ALIAS", "default");
            crate::power_control::PowerControlBackend::External(
                crate::power_control::ExternalControl::new(dev_ctl, dut_alias, channels),
            )
        } else {
            crate::power_control::PowerControlBackend::Ch340(
                crate::power_control::Ch340RelayControl::new(
                    self.relay_host.clone(),
                    self.relay.port(),
                    channels,
                ),
            )
        }
    }

    /// Send raw bytes to the serial port — no markers, no command wrapping.
    /// Data goes to write channel; drained by read loop on next iteration.
    pub fn send_raw(&mut self, data: &str) -> serde_json::Value {
        let bytes = data.as_bytes().to_vec();
        let len = bytes.len();
        let _ = self.console.write_sender().try_send(bytes);
        serde_json::json!({"success": true, "bytes_sent": len})
    }

    /// Pause the serial engine — stops read loop, watchdog, and data sending.
    pub fn pause(&mut self) -> serde_json::Value {
        self.paused = true;
        self.console.close(); // Release TCP so nc can connect
        self.state
            .transition(crate::state_manager::TargetState::Dutabo);
        serde_json::json!({"success": true, "paused": true, "state": "dutabo", "message": "Serial released for dutabo."})
    }

    /// Resume the serial engine after pause.
    pub fn resume(&mut self) -> serde_json::Value {
        self.paused = false;
        // Keep dutabo state until reconnect succeeds (avoids disconnected flash)
        // read_loop_iter will handle the actual reconnection
        serde_json::json!({"success": true, "paused": false, "message": "Serial engine resuming."})
    }

    /// Run hardware reset learning cycles inline (holds engine lock).
    async fn run_hardware_learning(&mut self, learner: &ConnectionLearner) -> LearnResult {
        let cfg = learner.config();
        let num_cycles = cfg.cycles;
        let mut cycles = Vec::with_capacity(num_cycles);
        let learn_dir = &cfg.learn_dir;
        std::fs::create_dir_all(learn_dir).ok();

        for i in 0..num_cycles {
            let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let log_path = learn_dir.join(format!("learn-{ts}-{}.log", i + 1));

            // Phase 1: reset via the configured power-control backend.
            self.state.transition(TargetState::Booting);
            self.logs.flush_boot_log();
            self.logs.mark_boot_start();
            self.detector.reset_cycle();
            let mut backend = self.build_power_control();
            if !backend.has_button(Button::Reset) {
                return LearnResult {
                    connected: false,
                    cycles,
                    best_similarity: 0.0,
                    reference_log: None,
                    method: LearnMethod::HardwareReset,
                    relay_verified: false,
                    error: Some("Reset button/control not configured".into()),
                };
            }
            if let Err(e) = backend.pulse(Button::Reset, cfg.reset_pulse_ms).await {
                return LearnResult {
                    connected: false,
                    cycles,
                    best_similarity: 0.0,
                    reference_log: None,
                    method: LearnMethod::HardwareReset,
                    relay_verified: false,
                    error: Some(format!("Reset control failed via {}: {e}", backend.name())),
                };
            }

            // Phase 2: wait briefly for boot start, then capture.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let captured = self
                .capture_boot_output(learner.config().capture_timeout_secs)
                .await;

            // Write learn log
            std::fs::write(&log_path, &captured).ok();

            let full_content = String::from_utf8_lossy(&captured).to_string();
            let first_50 = ConnectionLearner::extract_first_n_lines(
                &full_content,
                learner.config().compare_lines,
            );
            let normalized = ConnectionLearner::normalize(&first_50);

            cycles.push(crate::connection_learner::LearnCycle {
                log_path,
                first_50: normalized,
                full_content,
                method: LearnMethod::HardwareReset,
            });

            tracing::info!(
                "Hardware learn cycle {}/{}: {} bytes captured",
                i + 1,
                3,
                captured.len()
            );
        }

        learner.evaluate(cycles, LearnMethod::HardwareReset)
    }

    /// Run software reboot learning cycles inline.
    async fn run_software_learning(&mut self, learner: &ConnectionLearner) -> LearnResult {
        let cfg = learner.config();
        let num_cycles = cfg.cycles;
        let mut cycles = Vec::with_capacity(num_cycles);
        let learn_dir = &cfg.learn_dir;
        std::fs::create_dir_all(learn_dir).ok();

        for i in 0..num_cycles {
            let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let log_path = learn_dir.join(format!("learn-{ts}-{}.log", i + 1));

            // Phase 1: send reboot
            self.state.transition(TargetState::Booting);
            self.console.sendline("reboot");
            self.console.drain_writes().await;
            self.logs.flush_boot_log();
            self.logs.mark_boot_start();
            self.detector.reset_cycle();

            // Phase 2: wait for boot to settle then capture
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let captured = self
                .capture_boot_output(learner.config().capture_timeout_secs)
                .await;

            // Prepend "reboot" marker
            let mut full_data = b"reboot\n".to_vec();
            full_data.extend_from_slice(&captured);

            std::fs::write(&log_path, &full_data).ok();

            let full_content = String::from_utf8_lossy(&full_data).to_string();
            let first_50 = ConnectionLearner::extract_first_n_lines(
                &full_content,
                learner.config().compare_lines,
            );
            let normalized = ConnectionLearner::normalize(&first_50);

            cycles.push(crate::connection_learner::LearnCycle {
                log_path,
                first_50: normalized,
                full_content,
                method: LearnMethod::SoftwareReboot,
            });

            tracing::info!(
                "Software learn cycle {}/{}: {} bytes captured",
                i + 1,
                3,
                captured.len()
            );
        }

        learner.evaluate(cycles, LearnMethod::SoftwareReboot)
    }

    /// Run software reboot learning comparing each cycle against an existing reference log.
    async fn run_software_learning_with_ref(
        &mut self,
        learner: &ConnectionLearner,
        reference_text: &str,
    ) -> LearnResult {
        let cfg = learner.config();
        let num_cycles = cfg.cycles;
        let mut cycles = Vec::with_capacity(num_cycles);
        let learn_dir = &cfg.learn_dir;
        std::fs::create_dir_all(learn_dir).ok();

        let ref_normalized = ConnectionLearner::normalize(
            &ConnectionLearner::extract_first_n_lines(reference_text, cfg.compare_lines),
        );

        for i in 0..num_cycles {
            let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
            let log_path = learn_dir.join(format!("learn-{ts}-{}.log", i + 1));

            // Send reboot
            self.state.transition(TargetState::Booting);
            self.console.sendline("reboot");
            self.console.drain_writes().await;
            self.logs.flush_boot_log();
            self.logs.mark_boot_start();
            self.detector.reset_cycle();

            // Wait for boot then capture
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let captured = self.capture_boot_output(cfg.capture_timeout_secs).await;

            let mut full_data = b"reboot\n".to_vec();
            full_data.extend_from_slice(&captured);
            std::fs::write(&log_path, &full_data).ok();

            let full_content = String::from_utf8_lossy(&full_data).to_string();
            let first_50 =
                ConnectionLearner::extract_first_n_lines(&full_content, cfg.compare_lines);
            let normalized = ConnectionLearner::normalize(&first_50);

            cycles.push(crate::connection_learner::LearnCycle {
                log_path,
                first_50: normalized,
                full_content,
                method: LearnMethod::SoftwareReboot,
            });

            tracing::info!(
                "Software+ref learn cycle {}/{}: {} bytes captured",
                i + 1,
                num_cycles,
                captured.len()
            );
        }

        // Evaluate: compute similarity of each cycle against the reference
        let min_similarity = cycles
            .iter()
            .map(|c| crate::connection_learner::compute_similarity(&c.first_50, &ref_normalized))
            .fold(1.0f64, f64::min);

        let connected = min_similarity >= cfg.similarity_threshold;
        let reference_log = if connected {
            // Don't overwrite existing reference — it's our ground truth
            Some(cfg.reference_log.clone())
        } else {
            None
        };

        LearnResult {
            connected,
            cycles,
            best_similarity: min_similarity,
            reference_log,
            method: LearnMethod::SoftwareReboot,
            relay_verified: false,
            error: if !connected {
                Some(format!(
                    "Similarity {:.1}% below threshold {:.1}% against existing reference",
                    min_similarity * 100.0,
                    cfg.similarity_threshold * 100.0
                ))
            } else {
                None
            },
        }
    }

    /// Capture serial output until silence or max timeout.
    /// Returns all bytes read during the capture window.
    async fn capture_boot_output(&mut self, timeout_secs: f64) -> Vec<u8> {
        let mut captured = Vec::new();
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs_f64(timeout_secs);
        let silence_timeout = std::time::Duration::from_secs(5); // 5s silence = boot done

        let mut last_data = tokio::time::Instant::now();

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            let read_timeout = remaining.min(std::time::Duration::from_millis(500));
            match self.console.read_available(read_timeout, 4096).await {
                Ok(data) if !data.is_empty() => {
                    last_data = tokio::time::Instant::now();
                    let clean = crate::serial_engine::strip_ser2net_banner(&data);
                    if !clean.is_empty() {
                        self.logs.write(&clean);
                    }
                    self.state.on_activity();
                    let events = self.detector.feed(&clean);
                    self.handle_boot_events(events).await;
                    captured.extend_from_slice(&data);
                }
                Ok(_) => {
                    // No data — check silence timeout
                    if last_data.elapsed() > silence_timeout {
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        captured
    }

    /// Format LearnResult into JSON.
    fn format_learn_result(result: &LearnResult) -> serde_json::Value {
        let cycles_json: Vec<serde_json::Value> = result
            .cycles
            .iter()
            .map(|c| {
                serde_json::json!({
                    "log_path": c.log_path.to_string_lossy(),
                    "method": match c.method {
                        LearnMethod::HardwareReset => "hardware_reset",
                        LearnMethod::SoftwareReboot => "software_reboot",
                    },
                    "size_bytes": c.full_content.len(),
                })
            })
            .collect();

        let method_str = match result.method {
            LearnMethod::HardwareReset => "hardware_reset",
            LearnMethod::SoftwareReboot => "software_reboot",
        };

        let mut resp = serde_json::json!({
            "success": result.connected,
            "connected": result.connected,
            "method": method_str,
            "best_similarity": format!("{:.1}%", result.best_similarity * 100.0),
            "similarity_threshold": "93.0%",
            "cycles": cycles_json,
            "relay_verified": result.relay_verified,
        });

        if let Some(ref path) = result.reference_log {
            resp["reference_log"] = serde_json::Value::String(path.to_string_lossy().to_string());
        }
        if let Some(ref err) = result.error {
            resp["error"] = serde_json::Value::String(err.clone());
        }

        resp
    }
}

/// Wait for a watcher pattern with optional force_prompt_wait (lava semantics).
///
/// This is a **free function** — no engine lock is held during the `.await`.
/// The caller owns the watcher lifecycle (add watcher before calling, remove after).
/// On timeout with `probe_on_timeout=true`, sends `"\n"` via `console_tx`
/// to provoke a fresh prompt, retrying up to 6 times at `timeout/10` each.
/// Mirrors lava's `force_prompt_wait` (shell.py:332).
pub async fn wait_pattern_with_probe(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<
        crate::boot_detector::WatcherMatch,
    >,
    timeout: f64,
    probe_on_timeout: bool,
    console_tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
) -> serde_json::Value {
    let max_probes = if probe_on_timeout { 6usize } else { 0 };
    let partial = if probe_on_timeout {
        timeout / 10.0
    } else {
        timeout
    };
    let mut elapsed = 0.0f64;
    let mut probes = 0usize;

    loop {
        let remaining = (timeout - elapsed).max(0.0);
        let wait = partial.min(remaining);
        let result =
            tokio::time::timeout(std::time::Duration::from_secs_f64(wait), rx.recv()).await;

        match result {
            Ok(Some(m)) => {
                return serde_json::json!({
                    "matched": true,
                    "matched_line": m.line,
                    "pattern_index": m.pattern_index,
                });
            }
            Ok(None) => {
                return serde_json::json!({
                    "matched": false,
                    "matched_line": null,
                });
            }
            Err(_) => {
                elapsed += wait;
                if elapsed >= timeout || probes >= max_probes {
                    return serde_json::json!({
                        "matched": false,
                        "matched_line": null,
                        "probes_sent": probes,
                    });
                }
                // Provoke a fresh prompt (lava force_prompt_wait).
                probes += 1;
                tracing::info!(
                    "wait_pattern timeout {:.1}s, probing with newline (attempt {}/{})",
                    elapsed,
                    probes,
                    max_probes
                );
                let _ = console_tx.try_send(b"\n".to_vec());
            }
        }
    }
}

/// 过滤 ser2net 连接 banner
///
/// Handles cross-chunk splits: if a ser2net banner line is split across two
/// read() boundaries, a partial-line buffer stores the trailing incomplete
/// line and prepends it to the next call, so the full line is filtered.
fn strip_ser2net_banner(data: &[u8]) -> Vec<u8> {
    use std::sync::LazyLock;
    use std::sync::Mutex;
    static PARTIAL: LazyLock<Mutex<Vec<u8>>> = LazyLock::new(|| Mutex::new(Vec::new()));

    let mut partial = PARTIAL.lock().unwrap();

    // Prepend any buffered partial line from the previous chunk.
    let combined = if partial.is_empty() {
        data.to_vec()
    } else {
        let mut c = std::mem::take(&mut *partial);
        c.extend_from_slice(data);
        c
    };

    let text = String::from_utf8_lossy(&combined);
    let mut lines: Vec<&str> = text.lines().collect();

    // If the last byte of the input is NOT '\n', the last line is incomplete.
    // Save it for the next call so it can be joined with the continuation.
    if !combined.is_empty() && combined.last() != Some(&b'\n') {
        if let Some(last) = lines.pop() {
            partial.extend_from_slice(last.as_bytes());
        }
    } else {
        partial.clear();
    }

    lines
        .into_iter()
        .filter(|l| !l.contains("ser2net port"))
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

/// 去除 Android kernel log 前缀 `[ 1234.567890][  T123]`
fn strip_android_klog(data: &[u8]) -> Vec<u8> {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::bytes::Regex> =
        LazyLock::new(|| regex::bytes::Regex::new(r"(?m)^\[\s*\d+\.\d+\]\[\s*T\d+\]\s*").unwrap());
    RE.replace_all(data, b"" as &[u8]).into_owned()
}

/// Engine wrapper for shared access between read loop and MCP handler
pub type SharedEngine = Arc<Mutex<SerialEngine>>;

pub fn new_shared_engine(config: Config) -> SharedEngine {
    Arc::new(Mutex::new(SerialEngine::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigFormat;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn create_test_config(project_dir: &std::path::Path) -> Config {
        let mut values = HashMap::new();
        values.insert("DEV_HOST_IP".into(), "127.0.0.1".into());
        values.insert("SERIAL_PORT".into(), "59999".into()); // unused port
        values.insert("RELAY_PORT".into(), "0".into());
        values.insert("RESET_CHANNEL".into(), "0".into());
        values.insert("MASKROM_CHANNEL".into(), "0".into());
        values.insert("HANG_TIMEOUT".into(), "60".into());
        values.insert("HANG_HYSTERESIS".into(), "3".into());
        values.insert("MAX_ARCHIVED_LOGS".into(), "10".into());
        values.insert("MAX_LOG_FILE_SIZE".into(), "100".into());
        values.insert("DUT_DIR".into(), ".dut-serial".into());
        values.insert("LOCK_DIR".into(), "/tmp/embedded-debug-test-locks".into());
        values.insert("LOGIN_USER".into(), "root".into());
        values.insert("LOGIN_PASS".into(), "".into());
        values.insert("UBOOT_INTERRUPT_STRATEGY".into(), "lava".into());

        Config {
            values,
            config_path: None,
            project_dir: Some(project_dir.to_path_buf()),
            format: crate::config::ConfigFormat::None,
        }
    }

    #[tokio::test]
    async fn test_engine_new() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let engine = SerialEngine::new(config);

        assert!(!engine.console.is_open());
        assert_eq!(engine.login_user, "root");
    }

    #[tokio::test]
    async fn test_get_state_dict() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let engine = SerialEngine::new(config);

        let state = engine.get_state_dict();
        assert_eq!(state["state"], ""); // Stopped → None → ""
        assert_eq!(state["boot_number"], 0);
        assert_eq!(state["relay_configured"], false);
        assert_eq!(state["login_configured"], true);
    }

    #[tokio::test]
    async fn test_read_log_empty() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let engine = SerialEngine::new(config);

        let result = engine.read_log(0, 50, None);
        assert_eq!(result["content"], "");
        assert_eq!(result["filename"], "");
    }

    #[tokio::test]
    async fn test_list_logs_empty() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let engine = SerialEngine::new(config);

        let result = engine.list_logs();
        assert_eq!(result["archives"].as_array().unwrap().len(), 0);
        assert_eq!(result["current"], "");
    }

    #[tokio::test]
    async fn test_rotate_log() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        // Open initial log
        engine.logs.open_current();

        let result = engine.rotate_log();
        assert_eq!(result["success"], true);
        assert!(!result["filename"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_poll_logs_no_log() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        let result = engine.poll_logs();
        assert_eq!(result["lines"].as_array().unwrap().len(), 0);
        assert_eq!(result["since"], 0);
    }

    #[tokio::test]
    async fn test_get_config() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let engine = SerialEngine::new(config);

        let cfg = engine.get_config();
        assert_eq!(cfg["DEV_HOST_IP"], "127.0.0.1");
        assert_eq!(cfg["SERIAL_PORT"], "59999");
    }

    #[tokio::test]
    async fn test_engine_with_logs() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        engine.logs.open_current();
        engine.logs.write(b"test line 1\n");
        engine.logs.write(b"test line 2\n");
        engine.logs.flush_sync();

        let result = engine.read_log(0, 10, None);
        // Note: open_current writes a header line, so total_lines = 3 (header + 2 data lines)
        assert_eq!(result["total_lines"].as_u64().unwrap(), 3);
        assert!(result["content"].as_str().unwrap().contains("test line 1"));
    }

    #[tokio::test]
    async fn test_engine_poll_logs() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        engine.logs.open_current();
        engine.logs.write(b"line 1\n");
        engine.logs.write(b"line 2\n");
        engine.logs.flush_sync();

        // First poll should get all lines (including header)
        let result1 = engine.poll_logs();
        let lines1 = result1["lines"].as_array().unwrap().len();
        assert!(lines1 >= 2); // At least the 2 data lines (may include header)
        let since1 = result1["since"].as_u64().unwrap();
        assert!(since1 > 0);

        // Write more data
        engine.logs.write(b"line 3\n");
        engine.logs.flush_sync();

        // Second poll should get only new lines
        let result2 = engine.poll_logs();
        let lines2 = result2["lines"].as_array().unwrap().len();
        assert_eq!(lines2, 1); // Only the new line
        assert!(result2["since"].as_u64().unwrap() > since1);
    }

    #[tokio::test]
    async fn test_reset_target_no_relay() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        let result = engine.reset_target(false, 1, 1.0).await;
        assert_eq!(result["success"], false);
        assert_eq!(result["error"], "No reset control configured");
    }

    #[tokio::test]
    async fn test_enter_maskrom_no_relay() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        let result = engine.enter_maskrom().await;
        assert_eq!(result["success"], false);
        assert_eq!(result["error"], "No reset control configured");
    }

    #[tokio::test]
    async fn test_enter_maskrom_no_maskrom_channel() {
        let tmp = TempDir::new().unwrap();
        // Configure relay but with MASKROM_CHANNEL=0
        let mut values = HashMap::new();
        values.insert("DEV_HOST_IP".into(), "127.0.0.1".into());
        values.insert("SERIAL_PORT".into(), "59999".into());
        values.insert("RELAY_PORT".into(), "2001".into());
        values.insert("RESET_CHANNEL".into(), "2".into());
        values.insert("MASKROM_CHANNEL".into(), "0".into());
        values.insert("DUT_DIR".into(), ".dut-serial".into());
        values.insert("LOCK_DIR".into(), "/tmp/embedded-debug-test-locks".into());
        values.insert("LOGIN_USER".into(), "root".into());
        let config = Config {
            values,
            config_path: None,
            project_dir: Some(tmp.path().to_path_buf()),
            format: crate::config::ConfigFormat::None,
        };
        let mut engine = SerialEngine::new(config);

        let result = engine.enter_maskrom().await;
        assert_eq!(result["success"], false);
        assert_eq!(result["error"], "MASKROM control not configured");
    }

    #[tokio::test]
    async fn test_shared_engine() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let shared = new_shared_engine(config);

        // Should be able to lock and access
        {
            let engine = shared.lock().await;
            assert!(!engine.console.is_open());
        }

        // Should be able to lock again
        {
            let mut engine = shared.lock().await;
            engine.logs.open_current();
            assert_eq!(engine.logs.boot_number(), 1);
        }
    }

    #[tokio::test]
    async fn test_watchdog_once_not_running() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        // Should not panic even when not running
        engine.watchdog_once();
    }

    #[tokio::test]
    async fn test_read_loop_iter_not_running() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        // Should return immediately when not running
        engine.read_loop_iter().await;
    }

    /// Smoke test: queue_command returns a valid receiver and check_timeouts resolves it
    #[tokio::test]
    async fn test_queue_command_returns_receiver() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        // Set up a write_fn so commands can be sent
        let write_tx = engine.console.write_sender();
        engine.commands.set_write_fn(Box::new(move |data| {
            write_tx.try_send(data.to_vec()).ok();
        }));

        // queue_command should return a valid receiver
        let rx = engine.queue_command("echo test", 0.1); // 100ms timeout for fast test

        // Simulate read loop calling check_timeouts (normally done by read_loop_iter)
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        engine.commands.check_timeouts();

        let result = rx.await;
        assert!(result.is_ok());
        let cmd_result = result.unwrap();
        assert!(cmd_result.timed_out);
    }

    // ── state transition tests (no hardware) ─────────────────────────────

    #[tokio::test]
    async fn test_get_state_dict_fields() {
        let mut values = std::collections::HashMap::new();
        values.insert("DEV_HOST_IP".into(), "127.0.0.1".into());
        values.insert("SERIAL_PORT".into(), "2000".into());
        values.insert("LOGIN_USER".into(), "test".into());
        values.insert("DUT_ALIAS".into(), "test-dut".into());
        values.insert("LOCK_DIR".into(), "/tmp/test-locks".into());
        let cfg = Config {
            values,
            config_path: None,
            project_dir: Some(std::env::temp_dir()),
            format: ConfigFormat::None,
        };
        let engine = new_shared_engine(cfg);
        let eng = engine.lock().await;
        let state = eng.get_state_dict();
        assert!(state.get("state").is_some());
        assert!(state.get("login_configured").is_some());
        assert!(state.get("relay_configured").is_some());
        assert!(state.get("boot_number").is_some());
    }

    #[tokio::test]
    async fn test_engine_new_and_stop() {
        let mut values = std::collections::HashMap::new();
        values.insert("DEV_HOST_IP".into(), "127.0.0.1".into());
        values.insert("SERIAL_PORT".into(), "59999".into());
        values.insert("LOCK_DIR".into(), "/tmp/debug-console-test-locks".into());
        values.insert("LOGIN_USER".into(), "root".into());
        let cfg = Config {
            values,
            config_path: None,
            project_dir: Some(std::env::temp_dir()),
            format: crate::config::ConfigFormat::None,
        };
        // Engine should not panic on new()
        let engine = new_shared_engine(cfg);
        let mut eng = engine.lock().await;
        // start() will fail to connect (no ser2net at 127.0.0.1:59999) but that's OK
        let _result = eng.start().await;
        // Should either connect (if something on 59999) or transition to disconnected
        eng.stop().await;
    }

    #[test]
    fn test_target_state_display() {
        assert_eq!(TargetState::Active.as_str(), "active");
        assert_eq!(TargetState::Booting.as_str(), "booting");
        assert_eq!(TargetState::Crashed.as_str(), "crashed");
        assert_eq!(TargetState::Disconnected.as_str(), "disconnected");
        assert_eq!(TargetState::Stopped.as_str(), "stopped");
    }

    #[test]
    fn test_format_statusline_all_states() {
        let tmp = TempDir::new().unwrap();
        let sm = StateManager::new(tmp.path(), 60, 3, ".dut-serial", "");
        for state in &[
            TargetState::Active,
            TargetState::Booting,
            TargetState::UBoot,
            TargetState::Crashed,
            TargetState::Disconnected,
            TargetState::DutOff,
        ] {
            let text = sm.format_statusline(*state);
            assert!(!text.is_empty());
            assert!(text.contains('\x1b')); // ANSI color codes
        }
    }
}
