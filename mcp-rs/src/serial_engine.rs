//! SerialEngine — 核心引擎, 协调 console, log, detector, state, commands, relay。
//!
//! Lifespan: start → 获取锁 + 打开串口 + 启动读循环/看门狗
//!           stop  → 关闭一切 + 释放资源

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::boot_detector::{BootEvent, BootStageDetector};
use crate::command_queue::CommandQueue;
use crate::config::Config;
use crate::console::SerialConsoleDriver;
use crate::lock_manager;
use crate::log_manager::LogManager;
use crate::relay_manager::RelayManager;
use crate::state_manager::{StateManager, TargetState};

pub struct SerialEngine {
    pub console: SerialConsoleDriver,
    pub detector: BootStageDetector,
    pub state: StateManager,
    pub logs: LogManager,
    pub commands: CommandQueue,
    pub relay: RelayManager,
    config: Config,
    running: bool,
    read_handle: Option<tokio::task::JoinHandle<()>>,
    watchdog_handle: Option<tokio::task::JoinHandle<()>>,
    host: String,
    serial_target: String,
    login_user: String,
    login_pass: String,
    interrupt_strategy: String,
    /// poll_logs 的文件位置跟踪
    poll_position: u64,
}

impl SerialEngine {
    pub fn new(config: Config) -> Self {
        let project_dir = config
            .project_dir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        let host = config.dev_host_ip();
        let serial_target = config.serial_target();
        let dut_dir = config.dut_dir();
        // 提取所有需要的配置值（在 config 被 move 之前）
        let login_user = config.login_user();
        let login_pass = config.login_pass();
        let interrupt_strategy = config.interrupt_strategy();

        Self {
            console: SerialConsoleDriver::new(host.clone(), serial_target.clone()),
            detector: BootStageDetector::new(),
            state: StateManager::new(
                &project_dir,
                config.hang_timeout(),
                config.hang_hysteresis(),
                &dut_dir,
            ),
            logs: LogManager::new(
                &project_dir,
                config.max_archived_logs(),
                config.max_log_file_size_mb(),
                &dut_dir,
            ),
            commands: CommandQueue::new(),
            relay: RelayManager::new(
                host.clone(),
                config.relay_port(),
                config.reset_channel(),
                config.maskrom_channel(),
            ),
            config,
            running: false,
            read_handle: None,
            watchdog_handle: None,
            host,
            serial_target,
            login_user,
            login_pass,
            interrupt_strategy,
            poll_position: 0,
        }
    }

    /// 启动引擎: 项目单例检查 → 获取串口锁 → 打开串口 → 启动后台任务
    pub async fn start(&mut self) -> Result<(), String> {
        let project_dir = self.config.project_dir.clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        let dut_dir = self.config.dut_dir();

        // 1. 项目级单例: 同一 project_dir 只能有一个 MCP
        if let Some(conflicting_pid) = lock_manager::check_project_singleton(&project_dir, &dut_dir) {
            return Err(format!(
                "MCP already running for this project (PID {}).\n\
                 Only one MCP instance per project directory.\n\
                 Kill the existing process or use 'fuser -k 3000/tcp'.",
                conflicting_pid
            ));
        }

        // 2. 获取串口互斥锁
        let lock_dir = self.config.lock_dir();
        if let Some(conflicting_pid) = lock_manager::acquire_lock(&self.host, &self.serial_target, &lock_dir) {
            return Err(format!(
                "Target {}:{} is already in use by PID {}.",
                self.host, self.serial_target, conflicting_pid
            ));
        }

        // 3. 写入 PID 文件
        self.state.write_pid(&project_dir, &dut_dir);

        // 2. 设置 write_fn for CommandQueue
        let write_tx = self.console.write_sender();
        self.commands.set_write_fn(Box::new(move |data| {
            write_tx.send(data.to_vec()).ok();
        }));

        // 3. 初始化日志缓冲
        self.logs.mark_boot_start();

        // 4. 连接串口 + 探测初始状态
        match self.console.connect().await {
            Ok(()) => {
                tracing::info!("Serial connected to {}:{}", self.host, self.serial_target);
                self.probe_initial_state().await;
            }
            Err(e) => {
                tracing::warn!("Cannot open serial: {e}");
                self.state.transition(TargetState::Disconnected);
            }
        }

        // 5. 启动后台任务
        self.running = true;

        tracing::info!("[{}:{}] SerialEngine started", self.host, self.serial_target);
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
        let project_dir = self.config.project_dir.clone()
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
    pub async fn stop(&mut self) {
        self.running = false;
        if let Some(h) = self.read_handle.take() {
            h.abort();
        }
        if let Some(h) = self.watchdog_handle.take() {
            h.abort();
        }
        self.console.close();
        self.relay.close();
        self.logs.close();
        let lock_dir = self.config.lock_dir();
        lock_manager::release_lock(&self.host, &self.serial_target, &lock_dir);
        self.state.transition(TargetState::Stopped);
        tracing::info!("[{}:{}] SerialEngine stopped", self.host, self.serial_target);
    }

    /// 检查事件列表中是否包含启动完成信号
    fn is_boot_complete(events: &[BootEvent]) -> bool {
        events.iter().any(|e| matches!(e,
            BootEvent::Stage(s) if matches!(s.as_str(),
                "shell" | "android_shell" | "android_adbd"
                | "android_bootanim" | "android_surfaceflinger"
                | "android_boot_completed"
            )
        ))
    }

    /// 启动后探测串口当前状态
    async fn probe_initial_state(&mut self) {
        match self
            .console
            .read_available(Duration::from_secs(1), 4096)
            .await
        {
            Ok(data) if !data.is_empty() => {
                let filtered = strip_ser2net_banner(&data);
                let probe_data = if !filtered.is_empty() { &filtered } else { &data };
                if !filtered.is_empty() {
                    self.logs.write(probe_data);
                }
                let events = self.detector.feed(probe_data);

                // 明确的启动完成信号 → active
                if Self::is_boot_complete(&events) {
                    self.logs.flush_boot_log();
                    self.logs.mark_boot_start();
                    self.state.transition(TargetState::Active);
                    tracing::info!("Probe: boot complete → active");
                    return;
                }
                if events.iter().any(|e| matches!(e, BootEvent::BootStart)) {
                    tracing::info!("Probe: SPL → booting");
                    return;
                }
                // 发换行探测 shell
                self.console.sendline("");
                self.console.drain_writes().await; // 确保换行通过 TCP 发出
                tokio::time::sleep(Duration::from_millis(500)).await;
                if let Ok(d2) = self.console.read_available(Duration::from_millis(800), 4096).await {
                    if !d2.is_empty() {
                        self.logs.write(&d2);
                        let e2 = self.detector.feed(&d2);
                        if Self::is_boot_complete(&e2) {
                            self.logs.flush_boot_log();
                            self.logs.mark_boot_start();
                            self.state.transition(TargetState::Active);
                            tracing::info!("Probe: 2nd pass boot complete → active");
                        } else if e2.iter().any(|e| matches!(e, BootEvent::Stage(s) if s == "uboot")) {
                            self.state.transition(TargetState::UBoot);
                            tracing::info!("Probe: at U-Boot prompt");
                        } else {
                            self.logs.flush_boot_log();
                            self.logs.mark_boot_start();
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
            Ok(_) | Err(_) => {
                self.state.transition(TargetState::Active);
                tracing::info!("Probe: no data → active");
            }
        }
        // 探测完成后初始化日志缓冲 (match 外部)
        if self.state.current() == TargetState::Active {
            self.logs.mark_boot_start();
        }
    }

    /// 读取并处理串口数据 (调用前确保 wait_readable 已返回 true)
    pub async fn process_serial_data(&mut self) {
        self.console.drain_writes().await;
        match self.console.read_available(Duration::from_millis(0), 4096).await {
            Ok(data) if !data.is_empty() => {
                self.state.on_activity();
                if self.state.current() == TargetState::DutOff {
                    self.state.transition(TargetState::Active);
                }
                let clean_data = strip_ser2net_banner(&data);
                if !clean_data.is_empty() { self.logs.write(&clean_data); }
                let events = self.detector.feed(&data);
                self.handle_boot_events(events).await;
                let clean = strip_android_klog(&data);
                self.commands.feed_serial_data(&clean);
            }
            Ok(_) => { self.commands.check_timeouts(); }
            Err(e) => { self.handle_read_error(e).await; }
        }
    }

    /// 检查连接状态, 断连则重连 (同步版本, 由 watchdog 调用)
    pub fn try_reconnect(&mut self) {
        if !self.console.is_open() {
            tracing::info!("Connection lost, will reconnect on next read loop iteration");
            // 实际重连由 read_loop_iter 的 disconnected 路径处理
        }
    }

    /// 事件驱动读循环: 等待数据 (epoll/kqueue, 无轮询), 处理 watchdog
    pub async fn read_loop_iter(&mut self) {
        if !self.running {
            return;
        }
        self.console.drain_writes().await;
        // disconnected 状态 → 主动尝试重连
        if self.state.current() == TargetState::Disconnected {
            if let Ok(()) = self.console.connect().await {
                self.probe_initial_state().await;
                tracing::info!("Auto-reconnected from disconnected");
            } else {
                // 重连失败, 短暂等待后让 watchdog 再试
                tokio::time::sleep(Duration::from_secs(2)).await;
                return;
            }
        }
        // 事件驱动等待数据 (100ms 超时, 快速释放锁给 HTTP)
        if !self.console.wait_readable(Duration::from_millis(100)).await {
            return;
        }
        // 数据就绪, 读取处理
        match self.console.read_available(Duration::from_millis(0), 4096).await {
            Ok(data) if !data.is_empty() => {
                self.state.on_activity();
                if self.state.current() == TargetState::DutOff {
                    self.state.transition(TargetState::Active);
                    tracing::info!("Device resumed from DUT-off → active");
                }
                let clean_data = strip_ser2net_banner(&data);
                if !clean_data.is_empty() {
                    self.logs.write(&clean_data);
                }
                let events = self.detector.feed(&data);
                self.handle_boot_events(events).await;
                let clean = strip_android_klog(&data);
                self.commands.feed_serial_data(&clean);
            }
            Ok(_) => {
                self.commands.check_timeouts();
            }
            Err(e) => {
                self.handle_read_error(e).await;
            }
        }
    }

    async fn handle_read_error(&mut self, e: std::io::Error) {
        tracing::warn!("Serial read error: {e}");
        let cur = self.state.current();
        // 连接断开 → disconnected (ser2net 挂了, 板子可能正常)
        // hang 由 watchdog 的 check_hang 检测, 不在此判断
        self.state.transition(TargetState::Disconnected);
        // 尝试重连
        tracing::info!("Attempting reconnect (was {:?})...", cur);
        match self.console.connect().await {
            Ok(()) => {
                self.probe_initial_state().await;
                tracing::info!("Serial reconnected → resuming from {:?}", cur);
            }
            Err(e) => tracing::debug!("Reconnect failed: {e}"),
        }
    }

    /// 处理一次读循环迭代 (由 MCP server 主循环调用) — 保留兼容
    pub async fn read_once(&mut self) {
        if !self.running {
            return;
        }

        // 先处理待发写请求
        self.console.drain_writes().await;

        // 读取串口数据
        match self
            .console
            .read_available(Duration::from_millis(200), 4096)
            .await
        {
            Ok(data) if !data.is_empty() => {
                self.state.on_activity();
                // DUT-off → active: 设备唤醒/恢复通信
                if self.state.current() == TargetState::DutOff {
                    self.state.transition(TargetState::Active);
                    tracing::info!("Device resumed from DUT-off → active");
                }
                // 写入日志 (过滤 ser2net banner)
                let clean_data = strip_ser2net_banner(&data);
                if !clean_data.is_empty() {
                    self.logs.write(&clean_data);
                }
                // 检测启动阶段
                let events = self.detector.feed(&data);
                // 处理事件
                self.handle_boot_events(events).await;
                // 送入命令队列
                // 去除 Android kernel log 前缀后送入命令队列
                let clean = strip_android_klog(&data);
                self.commands.feed_serial_data(&clean);
            }
            Ok(_) => {
                // 超时，无数据
                self.commands.check_timeouts();
            }
            Err(e) => {
                self.handle_read_error(e).await;
            }
        }
    }

    /// 看门狗迭代 (由 MCP server 主循环定期调用)
    pub fn watchdog_once(&mut self) {
        if !self.running {
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
                    if matches!(s.as_str(), "shell" | "android_shell" | "android_adbd"
                        | "android_bootanim" | "android_surfaceflinger" | "android_boot_completed") {
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
                let recent = &self.logs.ring_buffer[self.logs.ring_buffer.len().saturating_sub(2048)..];
                let text = String::from_utf8_lossy(recent);
                if learner.is_boot_like(&text, 0.10) {
                    if cur == TargetState::Active {
                        self.state.transition(TargetState::Booting);
                        tracing::info!("Text similarity: reboot detected → booting");
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
                    // 保存上一个启动周期的日志, 开始新的缓冲
                    let cur = self.state.current();
                    match cur {
                        TargetState::Booting | TargetState::UBoot => {}
                        _ => {
                            self.logs.flush_boot_log();
                            self.logs.mark_boot_start();
                            tracing::info!("BootStart: new boot cycle (was {:?})", cur);
                        }
                    }
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
                        tracing::info!("Sending username: {}", self.login_user);
                        self.console.sendline(&self.login_user);
                    }
                }
                BootEvent::PasswordPrompt => {
                    if !self.login_pass.is_empty() {
                        tracing::info!("Sending password");
                        self.console.sendline(&self.login_pass);
                    }
                }
                BootEvent::Crash(crash_type, line) => {
                    self.state.transition(TargetState::Crashed);
                    tracing::warn!("CRASH [{crash_type}]: {line}");
                }
                BootEvent::Stage(stage) => {
                    let new_state = match stage.as_str() {
                        "uboot" | "autoboot" => Some(TargetState::UBoot),
                        "shell" | "android_shell" | "android_adbd"
                        | "android_bootanim" | "android_surfaceflinger"
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
    pub fn queue_command(&mut self, command: &str, timeout: f64) -> tokio::sync::oneshot::Receiver<crate::command_queue::CommandResult> {
        self.commands.execute(command.to_string(), timeout)
    }

    pub async fn send_command(&mut self, command: &str, timeout: f64) -> serde_json::Value {
        // reboot/shutdown: 直接发送, 不等待 marker 响应 (板子会重启)
        if command.trim() == "reboot" || command.trim() == "poweroff" || command.trim() == "shutdown" {
            self.console.sendline(command);
            serde_json::json!({"output": "reboot sent", "exit_code": 0, "timed_out": false})
        } else {
            let rx = self.commands.execute(command.to_string(), timeout);
            match rx.await {
                Ok(result) => serde_json::json!({
                    "output": result.output,
                    "exit_code": result.exit_code,
                    "timed_out": result.timed_out,
                }),
                Err(_) => serde_json::json!({
                    "error": "Command cancelled",
                }),
            }
        }
    }

    /// 在 U-Boot 提示符下发送原始命令 (即发即返回, 不阻塞)
    pub async fn send_uboot_command(&mut self, command: &str, _timeout: f64) -> serde_json::Value {
        self.console.sendline(command);
        // 短暂等待命令回显 (最多 1s), 不阻塞 read loop
        let mut output = String::new();
        if let Ok(data) = self.console.read_available(std::time::Duration::from_millis(500), 1024).await {
            if !data.is_empty() {
                let clean = strip_ser2net_banner(&data);
                if !clean.is_empty() { self.logs.write(&clean); }
                self.state.on_activity();
                let events = self.detector.feed(&data);
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

    pub async fn reset_target(&mut self, wait_boot: bool) -> serde_json::Value {
        if !self.relay.configured() {
            return serde_json::json!({"success": false, "error": "No relay configured"});
        }
        // 立即切状态, 不等 relay 完成
        self.state.transition(TargetState::Booting);
        let ok = self.relay.reset().await;
        if ok {
            self.logs.flush_boot_log();
            self.logs.mark_boot_start();
            self.detector.reset_cycle();
            if wait_boot {
                let result = self.wait_pattern_internal("login:", 120.0).await;
                return serde_json::json!({
                    "success": true,
                    "new_boot_number": self.logs.boot_number(),
                    "log_path": self.logs.current_path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
                    "boot_complete": result["matched"],
                });
            }
        }
        serde_json::json!({
            "success": ok,
            "new_boot_number": self.logs.boot_number(),
            "log_path": self.logs.current_path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        })
    }

    /// 设置 enter_uboot 并返回 receiver (调用方 release lock 后 await)
    pub fn queue_enter_uboot(&mut self) -> tokio::sync::mpsc::UnboundedReceiver<String> {
        let pattern = r"=>|U-Boot[>#]".to_string();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.detector.add_watcher(&pattern, tx);
        rx
    }

    /// 执行 relay reset + Ctrl-C flood (持有 lock, 快速完成)
    pub async fn do_relay_reset_and_flood(&mut self) {
        self.state.transition(TargetState::Booting);
        if self.console.is_open() {
            let flood: Vec<u8> = vec![0x03; 100];
            self.console.write_raw(&flood).await;
        }
        self.relay.reset().await;
        self.logs.flush_boot_log();
        self.logs.mark_boot_start();
        self.detector.reset_cycle();
        for _ in 0..8 {
            if self.console.is_open() {
                let flood: Vec<u8> = vec![0x03; 100];
                self.console.write_raw(&flood).await;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub async fn enter_uboot(&mut self) -> serde_json::Value {
        if !self.relay.configured() {
            return serde_json::json!({"success": false, "error": "No relay configured"});
        }
        let mut rx = self.queue_enter_uboot();
        self.do_relay_reset_and_flood().await;
        // 释放锁后 await — 实际由 MCP handler 处理
        let matched = tokio::time::timeout(Duration::from_secs(20), rx.recv()).await;
        let pattern = r"=>|U-Boot[>#]";
        self.detector.remove_watcher_by_pattern(pattern);
        if let Ok(Some(_line)) = matched {
            self.state.transition(TargetState::UBoot);
            return serde_json::json!({"success": true, "state_after": "uboot"});
        }
        serde_json::json!({"success": false, "state_after": self.state.current().as_str(), "error": "Timed out waiting for U-Boot prompt"})
    }

    pub async fn wait_pattern(&mut self, pattern: &str, timeout: f64) -> serde_json::Value {
        let result = self.wait_pattern_internal(pattern, timeout).await;
        serde_json::json!({
            "matched": result["matched"],
            "matched_line": result["matched_line"],
            "elapsed_seconds": 0,
        })
    }

    async fn wait_pattern_internal(&mut self, pattern: &str, timeout: f64) -> serde_json::Value {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        self.detector.add_watcher(pattern, tx);

        let result = tokio::time::timeout(Duration::from_secs_f64(timeout), rx.recv()).await;

        self.detector.remove_watcher_by_pattern(pattern);

        match result {
            Ok(Some(line)) => serde_json::json!({
                "matched": true,
                "matched_line": line,
            }),
            _ => serde_json::json!({
                "matched": false,
                "matched_line": null,
            }),
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

    pub fn get_config(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (k, v) in &self.config.values {
            map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        serde_json::Value::Object(map)
    }
}

/// 过滤 ser2net 连接 banner
fn strip_ser2net_banner(data: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(data);
    text.lines()
        .filter(|l| !l.contains("ser2net port"))
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

/// 去除 Android kernel log 前缀 `[ 1234.567890][  T123]`
fn strip_android_klog(data: &[u8]) -> Vec<u8> {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
        regex::bytes::Regex::new(r"(?m)^\[\s*\d+\.\d+\]\[\s*T\d+\]\s*").unwrap()
    });
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
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn create_test_config(project_dir: &std::path::Path) -> Config {
        let mut values = HashMap::new();
        values.insert("RK_DEV_HOST_IP".into(), "127.0.0.1".into());
        values.insert("RK_SERIAL_PORT".into(), "59999".into()); // unused port
        values.insert("RK_RELAY_PORT".into(), "0".into());
        values.insert("RK_RESET_CHANNEL".into(), "0".into());
        values.insert("RK_MASKROM_CHANNEL".into(), "0".into());
        values.insert("RK_HANG_TIMEOUT".into(), "60".into());
        values.insert("RK_HANG_HYSTERESIS".into(), "3".into());
        values.insert("RK_MAX_ARCHIVED_LOGS".into(), "10".into());
        values.insert("RK_MAX_LOG_FILE_SIZE".into(), "100".into());
        values.insert("RK_DUT_DIR".into(), ".dut-serial".into());
        values.insert("RK_LOCK_DIR".into(), "/tmp/embedded-debug-test-locks".into());
        values.insert("RK_LOGIN_USER".into(), "root".into());
        values.insert("RK_LOGIN_PASS".into(), "".into());
        values.insert("RK_UBOOT_INTERRUPT_STRATEGY".into(), "lava".into());

        Config {
            values,
            config_path: None,
            project_dir: Some(project_dir.to_path_buf()),
        }
    }

    #[tokio::test]
    async fn test_engine_new() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let engine = SerialEngine::new(config);

        assert!(!engine.console.is_open());
        assert_eq!(engine.login_user, "root");
        assert_eq!(engine.interrupt_strategy, "lava");
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
        assert_eq!(cfg["RK_DEV_HOST_IP"], "127.0.0.1");
        assert_eq!(cfg["RK_SERIAL_PORT"], "59999");
    }

    #[tokio::test]
    async fn test_engine_with_logs() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        engine.logs.open_current();
        engine.logs.write(b"test line 1\n");
        engine.logs.write(b"test line 2\n");

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

        // First poll should get all lines (including header)
        let result1 = engine.poll_logs();
        let lines1 = result1["lines"].as_array().unwrap().len();
        assert!(lines1 >= 2); // At least the 2 data lines (may include header)
        let since1 = result1["since"].as_u64().unwrap();
        assert!(since1 > 0);

        // Write more data
        engine.logs.write(b"line 3\n");

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

        let result = engine.reset_target(false).await;
        assert_eq!(result["success"], false);
        assert_eq!(result["error"], "No relay configured");
    }

    #[tokio::test]
    async fn test_enter_uboot_no_relay() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        let result = engine.enter_uboot().await;
        assert_eq!(result["success"], false);
        assert_eq!(result["error"], "No relay configured");
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
    async fn test_read_once_not_running() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        // Should return immediately when not running
        engine.read_once().await;
    }

    /// Smoke test: reboot and reset must return in <3s
    #[tokio::test]
    async fn test_reboot_performance_within_3s() {
        let tmp = TempDir::new().unwrap();
        let config = create_test_config(tmp.path());
        let mut engine = SerialEngine::new(config);

        // Test soft reboot fast-path
        let start = std::time::Instant::now();
        let result = engine.send_command("reboot", 5.0).await;
        let elapsed = start.elapsed().as_millis();
        assert!(elapsed < 3000, "reboot took {}ms, expected <3000ms", elapsed);
        assert_eq!(result["output"], "reboot sent");
        assert_eq!(result["timed_out"], false);

        // Test poweroff fast-path
        let start = std::time::Instant::now();
        let result = engine.send_command("poweroff", 5.0).await;
        let elapsed = start.elapsed().as_millis();
        assert!(elapsed < 3000, "poweroff took {}ms, expected <3000ms", elapsed);

        // Test normal command still works (will timeout on test server)
        let result = engine.send_command("echo test", 1.0).await;
        assert!(result["timed_out"].as_bool().unwrap_or(true));
    }
}
