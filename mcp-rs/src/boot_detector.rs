//! Boot stage detector — 双模式: regex 硬匹配 + 文本相似度自适应
//!
//! Mode 1 (default): 预编译 regex 检测已知 SOC 启动阶段
//! Mode 2 (StageLearner): 从参考日志学习阶段锚点, 用文本相似度匹配新 SOC
//!
//! 用法:
//!   let learner = StageLearner::from_reference("ref-boot.log");
//!   let stage = learner.classify_line("U-Boot 2024.01 (Jan 01 2025)");

use std::collections::HashMap;

use once_cell::sync::Lazy;
use regex::bytes::Regex;

/// 启动阶段定义
struct BootStage {
    name: &'static str,
    pattern: &'static Regex,
    action: Option<&'static str>,
}

static BOOT_STAGES: Lazy<Vec<BootStage>> = Lazy::new(|| {
    vec![
        // Bootloader stages
        BootStage { name: "spl",      pattern: &SPL_RE,      action: Some("rotate_log") },
        BootStage { name: "tpl",      pattern: &TPL_RE,      action: None },
        BootStage { name: "bl31",     pattern: &BL31_RE,     action: None },
        BootStage { name: "optee",    pattern: &OPTEE_RE,    action: None },
        BootStage { name: "ddr",      pattern: &DDR_RE,      action: None },
        BootStage { name: "uboot",    pattern: &UBOOT_RE,    action: None },
        BootStage { name: "autoboot", pattern: &AUTOBOOT_RE, action: Some("send_ctrl_c") },
        // Kernel
        BootStage { name: "kernel",   pattern: &KERNEL_RE,   action: None },
        BootStage { name: "start",    pattern: &START_RE,    action: None },
        // Linux login/shell (Debian/Ubuntu)
        BootStage { name: "login",    pattern: &LOGIN_RE,    action: Some("send_login") },
        BootStage { name: "password", pattern: &PASSWD_RE,   action: Some("send_password") },
        BootStage { name: "shell",    pattern: &SHELL_RE,    action: None },
        // Android boot completion signals
        BootStage { name: "android_shell",    pattern: &ANDROID_SHELL_RE,    action: None },
        BootStage { name: "android_init",     pattern: &ANDROID_INIT_RE,     action: None },
        BootStage { name: "android_adbd",     pattern: &ANDROID_ADBD_RE,     action: None },
        BootStage { name: "android_bootanim", pattern: &ANDROID_BOOTANIM_RE, action: None },
        BootStage { name: "android_surfaceflinger", pattern: &ANDROID_SF_RE, action: None },
        BootStage { name: "android_boot_completed", pattern: &ANDROID_BOOTED_RE, action: None },
    ]
});

struct CrashPattern {
    pattern: &'static Regex,
    ctype: &'static str,
}

static CRASH_PATTERNS: Lazy<Vec<CrashPattern>> = Lazy::new(|| {
    vec![
        CrashPattern { pattern: &PANIC_RE,      ctype: "panic" },
        CrashPattern { pattern: &BUG_RE,        ctype: "BUG" },
        CrashPattern { pattern: &OOPS_RE,       ctype: "Oops" },
        CrashPattern { pattern: &UNABLE_RE,     ctype: "kernel-fault" },
        CrashPattern { pattern: &BUG_HANDLE_RE, ctype: "BUG" },
        CrashPattern { pattern: &SEGFAULT_RE,   ctype: "segfault" },
        CrashPattern { pattern: &END_TRACE_RE,  ctype: "end-trace" },
    ]
});

// ── 预编译 Regex ──
static SPL_RE: Lazy<Regex>      = Lazy::new(|| Regex::new(r"U-Boot\s+SPL").unwrap());
static TPL_RE: Lazy<Regex>      = Lazy::new(|| Regex::new(r"TL[123]\s").unwrap());
static BL31_RE: Lazy<Regex>     = Lazy::new(|| Regex::new(r"BL31:").unwrap());
static OPTEE_RE: Lazy<Regex>    = Lazy::new(|| Regex::new(r"OP-TEE").unwrap());
static DDR_RE: Lazy<Regex>      = Lazy::new(|| Regex::new(r"DDR\s+Version").unwrap());
static UBOOT_RE: Lazy<Regex>    = Lazy::new(|| Regex::new(r"(?:U-Boot\s+20\d{2}|^=>\s)").unwrap());
static AUTOBOOT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"Hit\s+(?:any\s+)?key\s+to\s+stop\s+autoboot").unwrap());
static KERNEL_RE: Lazy<Regex>   = Lazy::new(|| Regex::new(r"Linux\s+version").unwrap());
static START_RE: Lazy<Regex>    = Lazy::new(|| Regex::new(r"Starting\s+kernel").unwrap());
static LOGIN_RE: Lazy<Regex>    = Lazy::new(|| Regex::new(r"(?:.*\s)?login:\s*$").unwrap());
static PASSWD_RE: Lazy<Regex>   = Lazy::new(|| Regex::new(r"Password:\s*$").unwrap());
static SHELL_RE: Lazy<Regex>    = Lazy::new(|| Regex::new(r"(?:[#\$]\s*$|^\S+@\S+:\S*[#\$]\s)").unwrap());
// Android boot detection — 不要求行末，因为 Android 串口 shell prompt 与 kernel log 混排
static ANDROID_SHELL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"console:/\s*[#\$]").unwrap());
static ANDROID_INIT_RE: Lazy<Regex>  = Lazy::new(|| Regex::new(r"init:\s*(?:started service|starting service)").unwrap());
static ANDROID_ADBD_RE: Lazy<Regex>  = Lazy::new(|| Regex::new(r"adbd?\s+(?:.*?\s)?(?:starting|started|ready)").unwrap());
static ANDROID_BOOTANIM_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"bootanim\s+(?:service\s+)?(?:started|stopped|done)").unwrap());
static ANDROID_SF_RE: Lazy<Regex>    = Lazy::new(|| Regex::new(r"surfaceflinger\s+.*?(?:started|ready)").unwrap());
static ANDROID_BOOTED_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?:sys\.)?boot_completed\s*[=:]\s*(?:1|true|done)").unwrap());

static PANIC_RE: Lazy<Regex>    = Lazy::new(|| Regex::new(r"Kernel\s+panic\s*[-:]").unwrap());
static BUG_RE: Lazy<Regex>      = Lazy::new(|| Regex::new(r"BUG:\s").unwrap());
static OOPS_RE: Lazy<Regex>     = Lazy::new(|| Regex::new(r"Oops:\s").unwrap());
static UNABLE_RE: Lazy<Regex>   = Lazy::new(|| Regex::new(r"Unable\s+to\s+handle\s+kernel").unwrap());
static BUG_HANDLE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"BUG:\s+unable\s+to\s+handle").unwrap());
static SEGFAULT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"Segmentation\s+fault").unwrap());
static END_TRACE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"---\[\s*end\s+trace\s+[0-9a-f]+\s*\]---").unwrap());

/// 检测到的启动事件
#[derive(Debug)]
pub enum BootEvent {
    /// SPL/DDR 检测 → 日志切割 + 状态 booting
    BootStart,
    /// autoboot 倒计时 → 发 Ctrl-C
    Autoboot,
    /// login: 提示 → 发用户名
    LoginPrompt,
    /// Password: 提示 → 发密码
    PasswordPrompt,
    /// 内核崩溃 → (crash_type, line)
    Crash(String, String),
    /// 阶段变更 → stage_name
    Stage(String),
}

/// Temporary watcher (for `serial_wait_pattern`). Carries an optional
/// `pattern_index` so a multi-pattern wait can report *which* pattern matched
/// (mirrors labgrid's `expect([p1, p2, TIMEOUT])` return index).
struct Watcher {
    pattern: Regex,
    pattern_index: usize,
    sender: tokio::sync::mpsc::UnboundedSender<WatcherMatch>,
}

/// A watcher match result: which pattern index fired and the matching line.
#[derive(Debug, Clone)]
pub struct WatcherMatch {
    pub pattern_index: usize,
    pub line: String,
}

// ── StageLearner: 文本相似度自适应阶段匹配 ──

/// 阶段指纹: 锚定行 + 预计算 3-gram 哈希集合 (避免运行时重复计算)
#[derive(Debug, Clone)]
pub struct StageFingerprint {
    pub stage: String,
    pub anchor: String,
    #[allow(dead_code)]
    pub prefix: String,
    #[allow(dead_code)]
    pub suffix: String,
    /// 预计算的 anchor 3-gram 哈希值 (u64)
    anchor_grams: Vec<u64>,
}

/// 文本相似度阶段学习器
///
/// 从参考启动日志中提取每个阶段的"锚定行"作为指纹。
/// 对新日志的每一行, 计算与所有指纹的相似度, 选出最佳匹配阶段。
///
/// # Example
/// ```
/// let learner = StageLearner::from_reference("ref-soc-boot.log");
/// learner.classify("U-Boot 2024.01 (Jan 01 2025)"); // → "uboot"
/// ```
pub struct StageLearner {
    pub fingerprints: Vec<StageFingerprint>,
    /// 参考日志全文 (用于文本相似度比对)
    reference_text: String,
    /// 阶段名 → 匹配阈值 (相似度低于此值不匹配)
    thresholds: HashMap<String, f64>,
    /// 最近匹配的阶段 (用于顺序约束)
    last_stage: Option<String>,
    /// stage → order (0 = earliest)
    stage_order: HashMap<String, usize>,
    /// was a crash detected?
    crashed: bool,
}

impl StageLearner {
    /// 从参考日志文件构建学习器
    pub fn from_reference(path: &std::path::Path) -> Result<Self, String> {
        let data = std::fs::read(path).map_err(|e| format!("read: {e}"))?;
        let text = String::from_utf8_lossy(&data);
        Self::from_reference_text(&text)
    }

    /// 从参考日志文本构建学习器
    pub fn from_reference_text(text: &str) -> Result<Self, String> {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() < 10 {
            return Err("Too few lines for learning".into());
        }

        // 阶段定义: (stage_name, [anchor_patterns])
        let stage_defs: Vec<(&str, Vec<&str>)> = vec![
            ("ddr",    vec!["DDR ", "DDR Version", "DDR Init"]),
            ("spl",    vec!["U-Boot SPL", "SPL board init"]),
            ("bl31",   vec!["BL31:", "ARM Trusted Firmware"]),
            ("optee",  vec!["OP-TEE", "I/TC:"]),
            ("uboot",  vec!["U-Boot 20", "U-Boot 202"]),
            ("kernel", vec!["Linux version", "Booting Linux", "Starting kernel"]),
            ("init",   vec!["init: ", "init:]"]),
            ("shell",  vec!["console:/", "login:", "# $", "$ $"]),
            ("booted", vec!["boot_completed", "Boot completed"]),
        ];

        let crash_patterns = [
            "Kernel panic", "BUG:", "Oops:", "Unable to handle kernel",
            "Segmentation fault", "---[ end trace", "panic - not syncing",
        ];
        let mut fingerprints = Vec::new();
        let mut thresholds = HashMap::new();
        let mut stage_order = HashMap::new();

        for (order, (stage, patterns)) in stage_defs.iter().enumerate() {
            thresholds.insert(stage.to_string(), 0.45);
            stage_order.insert(stage.to_string(), order);

            for i in 0..lines.len() {
                let line = lines[i].trim();
                if line.is_empty() { continue; }
                for pat in patterns {
                    if line.contains(pat) {
                        let prefix = if i > 0 { lines[i-1].trim().to_string() } else { String::new() };
                        let suffix = if i + 1 < lines.len() { lines[i+1].trim().to_string() } else { String::new() };
                        fingerprints.push(StageFingerprint {
                            stage: stage.to_string(),
                            anchor: line.to_string(),
                            prefix,
                            suffix,
                            anchor_grams: compute_3gram_hashes(line),
                        });
                        break;
                    }
                }
            }
        }

        // Add crash fingerprints
        thresholds.insert("crash".to_string(), 0.50);
        stage_order.insert("crash".to_string(), 999); // crashes can happen anytime

        for i in 0..lines.len() {
            let line = lines[i].trim();
            if line.is_empty() { continue; }
            for pat in &crash_patterns {
                if line.contains(pat) {
                    let prefix = if i > 0 { lines[i-1].trim().to_string() } else { String::new() };
                    let suffix = if i + 1 < lines.len() { lines[i+1].trim().to_string() } else { String::new() };
                    fingerprints.push(StageFingerprint {
                        stage: "crash".to_string(),
                        anchor: line.to_string(),
                        prefix,
                        suffix,
                        anchor_grams: compute_3gram_hashes(line),
                    });
                    break;
                }
            }
        }

        if fingerprints.is_empty() {
            return Err("No stage fingerprints extracted from reference".into());
        }

        let reference_text = text.to_string();
        Ok(Self {
            fingerprints,
            reference_text,
            thresholds,
            last_stage: None,
            stage_order,
            crashed: false,
        })
    }

    /// 分类一行文本 -> 阶段名, 或 None (不明确)
    pub fn classify_line(&mut self, line: &str) -> Option<String> {
        let line = line.trim();
        if line.is_empty() { return None; }

        let mut best_stage: Option<String> = None;
        let mut best_score = 0.0;

        let line_grams = compute_3gram_hashes(line);
        for fp in &self.fingerprints {
            // 使用预计算 3-gram 加速 anchor 相似度
            let anchor_sim = jaccard_similarity(&line_grams, &fp.anchor_grams);
            let score = anchor_sim;

            if score > best_score {
                best_score = score;
                best_stage = Some(fp.stage.clone());
            }
        }

        // 阈值过滤
        let threshold = self.thresholds.get(best_stage.as_deref().unwrap_or("")).copied().unwrap_or(0.4);
        if best_score < threshold {
            return None;
        }

        // 顺序约束: 不允许倒退 (除非 crash)
        let stage = best_stage.as_deref()?;
        if stage == "crash" {
            self.crashed = true;
            self.last_stage = Some("crash".into());
            return Some("crash".into());
        }

        let cur_order = self.stage_order.get(stage).copied().unwrap_or(0);
        if let Some(ref last) = self.last_stage {
            let last_order = self.stage_order.get(last.as_str()).copied().unwrap_or(0);
            // 允许小幅倒退 (1 个阶段)
            if cur_order + 1 < last_order && !self.crashed {
                return None;
            }
        }

        self.last_stage = Some(stage.to_string());
        Some(stage.to_string())
    }

    /// 计算缓冲区与参考日志的文本相似度 (word-level Jaccard via strsim)
    pub fn buffer_similarity(&self, buffer: &str) -> f64 {
        if buffer.len() < 50 || self.reference_text.len() < 50 {
            return 0.0;
        }
        strsim::sorensen_dice(buffer, &self.reference_text)
    }

    /// 判断缓冲区是否像启动日志 (相似度 > 阈值)
    pub fn is_boot_like(&self, buffer: &str, threshold: f64) -> bool {
        self.buffer_similarity(buffer) > threshold
    }

    /// 重置状态 (新启动周期)
    pub fn reset(&mut self) {
        self.last_stage = None;
        self.crashed = false;
    }

    /// 导出指纹 (供调试/审查)
    pub fn export_fingerprints(&self) -> Vec<(String, String)> {
        self.fingerprints.iter().map(|fp| (fp.stage.clone(), fp.anchor.clone())).collect()
    }
}

/// 预计算字符串的 3-gram 哈希值 (用于高效 Jaccard 比较)
fn compute_3gram_hashes(s: &str) -> Vec<u64> {
    s.as_bytes().windows(3).map(|w| {
        ((w[0] as u64) << 16) | ((w[1] as u64) << 8) | (w[2] as u64)
    }).collect()
}

/// 使用预计算 3-gram 计算 Jaccard 相似度
fn jaccard_similarity(a_grams: &[u64], b_grams: &[u64]) -> f64 {
    if a_grams.is_empty() || b_grams.is_empty() { return 0.0; }
    let a_set: std::collections::HashSet<u64> = a_grams.iter().copied().collect();
    let b_set: std::collections::HashSet<u64> = b_grams.iter().copied().collect();
    let intersection = a_set.intersection(&b_set).count();
    let union = a_set.len() + b_set.len() - intersection;
    if union == 0 { return 0.0; }
    intersection as f64 / union as f64
}

pub struct BootStageDetector {
    line_buf: Vec<u8>,
    boot_detected: bool,
    login_sent: bool,
    password_sent: bool,
    last_crash_time: std::time::Instant,
    watchers: Vec<Watcher>,
    /// 可选: 文本相似度学习器，用于未知 SOC 自适应
    pub learner: Option<StageLearner>,
}

impl BootStageDetector {
    pub fn new() -> Self {
        Self {
            line_buf: Vec::new(),
            boot_detected: false,
            login_sent: false,
            password_sent: false,
            last_crash_time: std::time::Instant::now() - std::time::Duration::from_secs(10),
            watchers: Vec::new(),
            learner: None,
        }
    }

    /// 加载参考日志, 启用自适应阶段检测 (用于新 SOC)
    pub fn load_reference(&mut self, path: &std::path::Path) -> Result<(), String> {
        let learner = StageLearner::from_reference(path)?;
        tracing::info!("StageLearner loaded: {} fingerprints from {}", learner.fingerprints.len(), path.display());
        self.learner = Some(learner);
        Ok(())
    }

    /// Add a temporary watcher (for `serial_wait_pattern`). Single pattern;
    /// `pattern_index` is 0. Use `add_watcher_multi` to wait on several
    /// patterns at once and learn which one matched.
    pub fn add_watcher(&mut self, pattern: &str, sender: tokio::sync::mpsc::UnboundedSender<WatcherMatch>) {
        if let Ok(re) = Regex::new(pattern) {
            self.watchers.push(Watcher { pattern: re, pattern_index: 0, sender });
        }
    }

    /// Add a multi-pattern watcher (labgrid `expect([p1, p2, ...])` semantics).
    /// Each pattern is compiled independently and tagged with its index in
    /// `patterns`. When any pattern matches, the watcher fires once with
    /// `WatcherMatch { pattern_index, line }` and is then removed.
    /// Returns the list of indices that were actually registered (patterns
    /// that failed to compile are skipped).
    ///
    /// Currently used by tests; kept as a public API for future
    /// multi-pattern wait tools (e.g. `serial_wait_login` matching
    /// login/password/incorrect in one call).
    #[allow(dead_code)]
    pub fn add_watcher_multi(
        &mut self,
        patterns: &[&str],
        sender: tokio::sync::mpsc::UnboundedSender<WatcherMatch>,
    ) -> Vec<usize> {
        let mut registered = Vec::new();
        for (i, pat) in patterns.iter().enumerate() {
            if let Ok(re) = Regex::new(pat) {
                self.watchers.push(Watcher {
                    pattern: re,
                    pattern_index: i,
                    sender: sender.clone(),
                });
                registered.push(i);
            }
        }
        registered
    }

    /// Remove watchers matching the given pattern string.
    pub fn remove_watcher_by_pattern(&mut self, pattern: &str) {
        if let Ok(re) = Regex::new(pattern) {
            let re_str = re.as_str().to_string();
            self.watchers.retain(|w| w.pattern.as_str() != re_str);
        }
    }

    /// Remove all watchers sharing the same sender (identified by the
    /// sender's `same_channel` identity). Used to clean up a
    /// multi-pattern watcher group in one call.
    pub fn remove_watcher_group(&mut self, sample: &tokio::sync::mpsc::UnboundedSender<WatcherMatch>) {
        self.watchers.retain(|w| !w.sender.same_channel(sample));
    }

    /// 输入数据，返回检测到的事件列表
    pub fn feed(&mut self, data: &[u8]) -> Vec<BootEvent> {
        let mut events = Vec::new();
        self.line_buf.extend_from_slice(data);

        // 防止 buffer 无限增长
        if self.line_buf.len() > 65536 {
            let drain = self.line_buf.len() - 32768;
            self.line_buf.drain(..drain);
        }

        while self.line_buf.contains(&b'\n') || self.line_buf.contains(&b'\r') {
            if let Some((line, rest)) = self.split_line() {
                self.line_buf = rest; // ALWAYS advance buffer
                if line.is_empty() {
                    continue;
                }
                let line_str = String::from_utf8_lossy(&line).to_string();

                // 检查崩溃
                if let Some(ce) = self.check_crash(&line, &line_str) {
                    events.push(ce);
                }
                // 检查启动阶段
                events.extend(self.check_stages(&line));
                // 检查 watchers
                self.check_watchers(&line_str);
            } else {
                break;
            }
        }

        events
    }

    /// 强制处理缓冲区残留数据 (无 \n 结尾的行, 如 shell prompt)
    pub fn flush_line_buf(&mut self) -> Vec<BootEvent> {
        let mut events = Vec::new();
        if self.line_buf.is_empty() {
            return events;
        }
        // 将整个残留缓冲当作一行处理
        let line = self.line_buf.clone();
        self.line_buf.clear();
        if line.iter().all(|&b| b.is_ascii_whitespace()) {
            return events;
        }
        let trimmed: Vec<u8> = line.iter().copied()
            .skip_while(|b| b.is_ascii_whitespace())
            .collect();
        let line_str = String::from_utf8_lossy(&trimmed).to_string();
        if let Some(ce) = self.check_crash(&trimmed, &line_str) {
            events.push(ce);
        }
        events.extend(self.check_stages(&trimmed));
        self.check_watchers(&line_str);
        events
    }

    /// 完全重置 — 新上电周期开始时调用
    pub fn reset_cycle(&mut self) {
        self.boot_detected = false;
        self.login_sent = false;
        self.password_sent = false;
        if let Some(ref mut learner) = self.learner {
            learner.reset();
        }
    }

    // ── internal ──

    fn split_line(&mut self) -> Option<(Vec<u8>, Vec<u8>)> {
        let idx_n = self.line_buf.iter().position(|&b| b == b'\n');
        let idx_r = self.line_buf.iter().position(|&b| b == b'\r');

        let (idx, sep_len) = match (idx_n, idx_r) {
            (Some(n), Some(r)) if n < r => (n, 1),
            (Some(_n), Some(r)) => (r, 1),
            (Some(n), None) => (n, 1),
            (None, Some(r)) => (r, 1),
            (None, None) => return None,
        };

        let line = self.line_buf[..idx].to_vec();
        let rest = self.line_buf[idx + sep_len..].to_vec();
        // trim whitespace from line
        let trimmed: Vec<u8> = line
            .iter()
            .copied()
            .skip_while(|b| b.is_ascii_whitespace())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .skip_while(|b| b.is_ascii_whitespace())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Some((trimmed, rest))
    }

    fn check_stages(&mut self, line: &[u8]) -> Vec<BootEvent> {
        let mut events = Vec::new();

        // 1. Regex 精确匹配 (已知 SOC)
        for stage in BOOT_STAGES.iter() {
            if stage.pattern.is_match(line) {
                if let Some(ev) = self.handle_stage(stage, line) {
                    events.push(ev);
                }
            }
        }

        // 2. StageLearner 自适应回退 (未知 SOC, 有参考日志时)
        if events.is_empty() {
            if let Some(ref mut learner) = self.learner {
                let text = String::from_utf8_lossy(line);
                if let Some(stage_name) = learner.classify_line(&text) {
                    // 检查是否与已检测阶段重复 (SPL 去重逻辑)
                    if stage_name == "spl" && self.boot_detected {
                        return events;
                    }
                    if stage_name == "spl" {
                        self.boot_detected = true;
                        self.login_sent = false;
                        self.password_sent = false;
                        events.push(BootEvent::BootStart);
                    } else if stage_name == "uboot" && !self.boot_detected {
                        // U-Boot 出现但 SPL 没被检测到 → 也触发 BootStart
                        self.boot_detected = true;
                        self.login_sent = false;
                        self.password_sent = false;
                        events.push(BootEvent::BootStart);
                    }
                    events.push(BootEvent::Stage(stage_name));
                }
            }
        }

        events
    }

    fn handle_stage(&mut self, stage: &BootStage, _line: &[u8]) -> Option<BootEvent> {
        // SPL 去重: 同一个 boot cycle 只触发一次
        if stage.name == "spl" && self.boot_detected {
            return None;
        }
        if stage.name == "spl" {
            self.boot_detected = true;
            self.login_sent = false;
            self.password_sent = false;
        }

        match stage.action {
            Some("rotate_log") => {
                return Some(BootEvent::BootStart);
            }
            Some("send_ctrl_c") => {
                return Some(BootEvent::Autoboot);
            }
            Some("send_login") => {
                if !self.login_sent {
                    self.login_sent = true;
                    return Some(BootEvent::LoginPrompt);
                }
            }
            Some("send_password") => {
                if !self.password_sent {
                    self.password_sent = true;
                    return Some(BootEvent::PasswordPrompt);
                }
            }
            _ => {}
        }

        Some(BootEvent::Stage(stage.name.to_string()))
    }

    fn check_crash(&mut self, line: &[u8], line_str: &str) -> Option<BootEvent> {
        for cp in CRASH_PATTERNS.iter() {
            if cp.pattern.is_match(line) {
                let now = std::time::Instant::now();
                if now.duration_since(self.last_crash_time).as_secs_f64() > 2.0 {
                    self.last_crash_time = now;
                    return Some(BootEvent::Crash(cp.ctype.to_string(), line_str.to_string()));
                }
            }
        }
        None
    }

    fn check_watchers(&mut self, line: &str) {
        let mut to_remove = Vec::new();
        for (i, watcher) in self.watchers.iter().enumerate() {
            if watcher.pattern.is_match(line.as_bytes()) {
                let m = WatcherMatch {
                    pattern_index: watcher.pattern_index,
                    line: line.to_string(),
                };
                if watcher.sender.send(m).is_err() {
                    to_remove.push(i);
                }
            }
        }
        // Remove watchers whose receiver has been dropped.
        for i in to_remove.into_iter().rev() {
            self.watchers.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_event(events: &[BootEvent], variant_name: &str) -> bool {
        events.iter().any(|e| match (e, variant_name) {
            (BootEvent::BootStart, "BootStart") => true,
            (BootEvent::Autoboot, "Autoboot") => true,
            (BootEvent::LoginPrompt, "LoginPrompt") => true,
            (BootEvent::PasswordPrompt, "PasswordPrompt") => true,
            (BootEvent::Crash(_, _), "Crash") => true,
            (BootEvent::Stage(_), "Stage") => true,
            _ => false,
        })
    }

    fn get_stage_name(events: &[BootEvent]) -> Option<&str> {
        events.iter().find_map(|e| match e {
            BootEvent::Stage(name) => Some(name.as_str()),
            _ => None,
        })
    }

    #[test]
    fn test_spl_detection() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"U-Boot SPL 2024.01\n");

        // SPL triggers BootStart (not Stage("spl")) because it has "rotate_log" action
        assert!(has_event(&events, "BootStart"));
    }

    #[test]
    fn test_ddr_version_detection() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"DDR Version 1.08\n");

        assert_eq!(get_stage_name(&events), Some("ddr"));
    }

    #[test]
    fn test_uboot_detection() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"U-Boot 2024.01 (Jan 01 2024 - 00:00:00)\n");

        assert_eq!(get_stage_name(&events), Some("uboot"));
    }

    #[test]
    fn test_autoboot_detection() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"Hit any key to stop autoboot:  5\n");

        assert!(has_event(&events, "Autoboot"));
    }

    #[test]
    fn test_autoboot_alternate_format() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"Hit key to stop autoboot\n");

        assert!(has_event(&events, "Autoboot"));
    }

    #[test]
    fn test_linux_version_detection() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"Linux version 6.1.0 (gcc 12.2.0)\n");

        assert_eq!(get_stage_name(&events), Some("kernel"));
    }

    #[test]
    fn test_login_prompt_detection() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"debian login:\n");

        assert!(has_event(&events, "LoginPrompt"));
    }

    #[test]
    fn test_password_prompt_detection() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"Password:\n");

        assert!(has_event(&events, "PasswordPrompt"));
    }

    #[test]
    fn test_shell_prompt_detection() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"root@debian:~#\n");

        assert_eq!(get_stage_name(&events), Some("shell"));
    }

    #[test]
    fn test_crash_panic_detection() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"Kernel panic - not syncing: Fatal exception\n");

        assert!(has_event(&events, "Crash"));
        if let BootEvent::Crash(ctype, _) = &events[0] {
            assert_eq!(ctype, "panic");
        }
    }

    #[test]
    fn test_crash_bug_detection() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"BUG: unable to handle page fault\n");

        assert!(has_event(&events, "Crash"));
    }

    #[test]
    fn test_crash_oops_detection() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"Oops: 00000000 [#1] SMP\n");

        assert!(has_event(&events, "Crash"));
    }

    #[test]
    fn test_crash_throttle() {
        let mut detector = BootStageDetector::new();

        // First crash should be detected
        let events1 = detector.feed(b"Kernel panic - not syncing\n");
        assert!(has_event(&events1, "Crash"));

        // Second crash within 2 seconds should be throttled
        let events2 = detector.feed(b"Kernel panic - not syncing\n");
        assert!(!has_event(&events2, "Crash"));
    }

    #[test]
    fn test_spl_dedup() {
        let mut detector = BootStageDetector::new();

        // First SPL should trigger BootStart
        let events1 = detector.feed(b"U-Boot SPL 2024.01\n");
        assert!(has_event(&events1, "BootStart"));

        // Second SPL should not trigger BootStart again
        let events2 = detector.feed(b"U-Boot SPL 2024.01\n");
        assert!(!has_event(&events2, "BootStart"));
    }

    #[test]
    fn test_reset_cycle() {
        let mut detector = BootStageDetector::new();

        // Trigger SPL
        detector.feed(b"U-Boot SPL 2024.01\n");

        // Reset cycle
        detector.reset_cycle();

        // SPL should trigger BootStart again
        let events = detector.feed(b"U-Boot SPL 2024.01\n");
        assert!(has_event(&events, "BootStart"));
    }

    #[test]
    fn test_login_once() {
        let mut detector = BootStageDetector::new();

        // First login prompt
        let events1 = detector.feed(b"login:\n");
        assert!(has_event(&events1, "LoginPrompt"));

        // Second login prompt should not trigger
        let events2 = detector.feed(b"login:\n");
        assert!(!has_event(&events2, "LoginPrompt"));
    }

    #[test]
    fn test_multiline_feed() {
        let mut detector = BootStageDetector::new();

        let data = b"line 1\nline 2\nU-Boot SPL 2024.01\nline 3\n";
        let events = detector.feed(data);

        // SPL triggers BootStart
        assert!(has_event(&events, "BootStart"));
    }

    #[test]
    fn test_chunked_feed() {
        let mut detector = BootStageDetector::new();

        // Feed in chunks
        detector.feed(b"U-Boot ");
        detector.feed(b"SPL ");
        detector.feed(b"2024.01\n");

        // Should still detect SPL
        // (Note: detector buffers until newline)
    }

    #[test]
    fn test_empty_feed() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"");
        assert!(events.is_empty());
    }

    #[test]
    fn test_no_newline_buffer() {
        let mut detector = BootStageDetector::new();
        let events = detector.feed(b"partial line without newline");
        assert!(events.is_empty());
    }

    #[test]
    fn test_watcher_add_remove() {
        let mut detector = BootStageDetector::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        detector.add_watcher("test.*pattern", tx);
        detector.remove_watcher_by_pattern("test.*pattern");

        // Watcher should be removed
        let _events = detector.feed(b"test_pattern_line\n");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_watcher_matches() {
        let mut detector = BootStageDetector::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        detector.add_watcher("custom.*marker", tx);

        detector.feed(b"custom_marker_line\n");
        let received = rx.try_recv().unwrap();
        assert!(received.line.contains("custom_marker_line"));
        assert_eq!(received.pattern_index, 0);
    }

    #[test]
    fn test_watcher_multi_reports_index() {
        let mut detector = BootStageDetector::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        // Register three patterns; the second should match.
        let registered = detector.add_watcher_multi(
            &["never_matches_xyz", "login:", "panic"],
            tx.clone(),
        );
        assert_eq!(registered, vec![0, 1, 2]);

        detector.feed(b"some login: prompt\n");
        let received = rx.try_recv().unwrap();
        assert_eq!(received.pattern_index, 1);
        assert!(received.line.contains("login:"));
    }

    #[test]
    fn test_watcher_group_cleanup() {
        let mut detector = BootStageDetector::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        detector.add_watcher_multi(&["aaa", "bbb"], tx.clone());
        // Cleanup by the sample sender should remove both.
        detector.remove_watcher_group(&tx);

        detector.feed(b"aaa\nbbb\n");
        assert!(rx.try_recv().is_err(), "group cleanup should remove all watchers sharing the sender");
    }

    #[test]
    fn test_line_split_cr() {
        let mut detector = BootStageDetector::new();
        let _events = detector.feed(b"line1\r\nline2\r\n");

        // Should process both lines
        // (exact events depend on content)
    }

    #[test]
    fn test_boot_sequence() {
        let mut detector = BootStageDetector::new();

        let boot_log = b"\
U-Boot SPL 2024.01\r\n\
DDR Version 1.08\r\n\
BL31: v2.10.0\r\n\
U-Boot 2024.01\r\n\
Hit any key to stop autoboot:  5\r\n\
Starting kernel ...\r\n\
Linux version 6.1.0\r\n\
login:\r\n\
root@debian:~#\r\n\
";

        let events = detector.feed(boot_log);

        assert!(has_event(&events, "BootStart")); // SPL
        assert!(has_event(&events, "Autoboot"));
        assert!(has_event(&events, "LoginPrompt"));
        // Check that shell stage was detected (may not be the first Stage event)
        assert!(events.iter().any(|e| matches!(e, BootEvent::Stage(s) if s == "shell")));
    }
}
