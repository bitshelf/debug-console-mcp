//! Connection learner — 三次学习判定串口连接是否真实建立。
//!
//! # 硬件 reset 学习流程
//!
//! 1. 按下 reset → 创建学习日志文件
//! 2. 松开 reset → 捕获启动日志
//! 3. 重复三次
//! 4. 比较三个文件前 50 行文本相似度
//! 5. 三者两两相似度均 > 93% → 保留最后一次为 reference_log
//!
//! # 软件 reboot 学习流程 (fallback)
//!
//! 1. 发送 reboot → 创建学习日志 (第一行固定 "reboot")
//! 2. 捕获重启日志
//! 3. 重复三次
//! 4. 相似度 > 93% → 判定连接建立
//!
//! # 继电器不可用判定
//!
//! - 三次硬件 reset 后前 50 行相似度 < 10% → 判定 reset 继电器不可用
//! - 降级为软件 reboot 学习

use std::path::PathBuf;

/// Result of a single learning cycle.
#[derive(Debug, Clone)]
pub struct LearnCycle {
    /// Path to the captured log file.
    pub log_path: PathBuf,
    /// First 50 lines (normalized) for similarity comparison.
    pub first_50: String,
    /// Full captured content.
    pub full_content: String,
    /// Whether this was a hardware reset or software reboot.
    pub method: LearnMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LearnMethod {
    HardwareReset,
    SoftwareReboot,
}

/// Overall learning result.
#[derive(Debug)]
pub struct LearnResult {
    /// Whether the connection is verified.
    pub connected: bool,
    /// The three learning cycles.
    pub cycles: Vec<LearnCycle>,
    /// Best similarity score across all pairwise comparisons.
    pub best_similarity: f64,
    /// Path to the generated reference log (if learning succeeded).
    pub reference_log: Option<PathBuf>,
    /// Learning method used.
    pub method: LearnMethod,
    /// Whether relay is verified working.
    pub relay_verified: bool,
    /// Error message if learning failed.
    pub error: Option<String>,
}

/// Configuration for the learning process.
#[derive(Debug, Clone)]
pub struct LearnConfig {
    /// Directory for learning logs: `.dut-serial/learn/`
    pub learn_dir: PathBuf,
    /// Where to write the reference log on success.
    pub reference_log: PathBuf,
    /// Similarity threshold for success (0.0–1.0, default 0.93).
    pub similarity_threshold: f64,
    /// Similarity threshold below which relay is considered broken (default 0.10).
    pub relay_broken_threshold: f64,
    /// Number of learning cycles (default 3).
    pub cycles: usize,
    /// Number of lines to compare from the top of each log.
    pub compare_lines: usize,
    /// Relay reset pulse duration in ms (default 500).
    #[allow(dead_code)]
    pub reset_pulse_ms: u64,
    /// Post-reset capture duration in seconds (default 30).
    pub capture_timeout_secs: f64,
}

/// Compile-time defaults from Cargo.toml `[package.metadata.learn]`.
/// These are set by build.rs as env vars; if missing, use hardcoded fallbacks.
macro_rules! learn_default {
    ($env:expr, $default:expr) => {
        option_env!($env)
            .and_then(|s| s.parse().ok())
            .unwrap_or($default)
    };
}

impl Default for LearnConfig {
    fn default() -> Self {
        Self {
            learn_dir: PathBuf::from(".dut-serial/learn"),
            reference_log: PathBuf::from(".dut-serial/reference-boot.log"),
            similarity_threshold: learn_default!("LEARN_SIMILARITY_THRESHOLD", 0.93),
            relay_broken_threshold: learn_default!("LEARN_RELAY_BROKEN_THRESHOLD", 0.10),
            cycles: learn_default!("LEARN_CYCLES", 3),
            compare_lines: learn_default!("LEARN_COMPARE_LINES", 50),
            reset_pulse_ms: learn_default!("LEARN_RESET_PULSE_MS", 500),
            capture_timeout_secs: learn_default!("LEARN_CAPTURE_TIMEOUT_SECS", 30.0),
        }
    }
}

/// The connection learner orchestrates multi-cycle learning.
pub struct ConnectionLearner {
    config: LearnConfig,
}

impl ConnectionLearner {
    pub fn new(config: LearnConfig) -> Self {
        Self { config }
    }

    /// Access the learning configuration.
    pub fn config(&self) -> &LearnConfig {
        &self.config
    }

    /// Evaluate learning cycles (public entry point for inline learning in serial_engine).
    pub fn evaluate(&self, cycles: Vec<LearnCycle>, method: LearnMethod) -> LearnResult {
        self.evaluate_cycles(cycles, method)
    }

    /// Evaluate learning cycles: compute pairwise similarity, check thresholds.
    fn evaluate_cycles(&self, cycles: Vec<LearnCycle>, method: LearnMethod) -> LearnResult {
        if cycles.len() < 2 {
            return LearnResult {
                connected: false,
                cycles,
                best_similarity: 0.0,
                reference_log: None,
                method,
                relay_verified: false,
                error: Some("Need at least 2 cycles for comparison".into()),
            };
        }

        // Compute pairwise similarities
        let mut min_similarity = 1.0f64;
        for i in 0..cycles.len() {
            for j in (i + 1)..cycles.len() {
                let sim = compute_similarity(&cycles[i].first_50, &cycles[j].first_50);
                if sim < min_similarity {
                    min_similarity = sim;
                }
            }
        }

        let relay_verified = match method {
            LearnMethod::HardwareReset => min_similarity >= self.config.relay_broken_threshold,
            LearnMethod::SoftwareReboot => false, // software reboot has no relay
        };

        let connected = min_similarity >= self.config.similarity_threshold;

        // If successful, copy the last cycle's full log as reference
        let reference_log = if connected {
            let last = cycles.last().unwrap();
            if let Some(parent) = self.config.reference_log.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            match std::fs::write(&self.config.reference_log, &last.full_content) {
                Ok(()) => {
                    tracing::info!(
                        "Reference log written: {} ({} bytes)",
                        self.config.reference_log.display(),
                        last.full_content.len()
                    );
                    Some(self.config.reference_log.clone())
                }
                Err(e) => {
                    tracing::error!("Failed to write reference log: {e}");
                    None
                }
            }
        } else {
            None
        };

        let error = if !connected {
            Some(format!(
                "Similarity {:.1}% below threshold {:.1}%",
                min_similarity * 100.0,
                self.config.similarity_threshold * 100.0
            ))
        } else {
            None
        };

        LearnResult {
            connected,
            best_similarity: min_similarity,
            reference_log,
            method,
            relay_verified,
            error,
            cycles,
        }
    }

    /// Normalize text for similarity comparison:
    /// - Strip ANSI escape codes
    /// - Strip NUL bytes
    /// - Strip timestamps (e.g. `[    0.000000]`)
    /// - Compress whitespace
    /// - Lowercase
    pub fn normalize(text: &str) -> String {
        // Strip ANSI escapes
        let cleaned = strip_ansi(text);
        // Strip NUL bytes
        let cleaned = cleaned.replace('\0', "");
        // Strip kernel timestamps: [   12.345678]
        let re_ts = regex::Regex::new(r"\[\s*\d+\.\d+\]").unwrap();
        let cleaned = re_ts.replace_all(&cleaned, "").to_string();
        // Strip Android log timestamps: [ 1234.567890][  T123]
        let re_android_ts = regex::Regex::new(r"\[\s*\d+\.\d+\]\[\s*T\d+\]\s*").unwrap();
        let cleaned = re_android_ts.replace_all(&cleaned, "").to_string();
        // Compress whitespace
        let re_ws = regex::Regex::new(r"\s+").unwrap();
        let cleaned = re_ws.replace_all(&cleaned, " ").to_string();
        cleaned.trim().to_lowercase()
    }

    /// Extract the first `n` lines from text.
    pub fn extract_first_n_lines(text: &str, n: usize) -> String {
        text.lines().take(n).collect::<Vec<_>>().join("\n")
    }
}

/// Compute combined similarity score between two normalized texts.
///
/// `score = jaro_winkler * 0.6 + jaccard_3gram * 0.4`
pub fn compute_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let jaro = strsim::jaro_winkler(a, b);
    let jaccard = jaccard_3gram(a, b);
    jaro * 0.6 + jaccard * 0.4
}

/// Compute 3-gram Jaccard similarity between two strings.
fn jaccard_3gram(a: &str, b: &str) -> f64 {
    let a_grams: Vec<u64> = a
        .as_bytes()
        .windows(3)
        .map(|w| ((w[0] as u64) << 16) | ((w[1] as u64) << 8) | (w[2] as u64))
        .collect();
    let b_grams: Vec<u64> = b
        .as_bytes()
        .windows(3)
        .map(|w| ((w[0] as u64) << 16) | ((w[1] as u64) << 8) | (w[2] as u64))
        .collect();

    if a_grams.is_empty() || b_grams.is_empty() {
        return 0.0;
    }

    let a_set: std::collections::HashSet<u64> = a_grams.iter().copied().collect();
    let b_set: std::collections::HashSet<u64> = b_grams.iter().copied().collect();

    let intersection = a_set.intersection(&b_set).count();
    let union = a_set.len() + b_set.len() - intersection;
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

/// Strip ANSI escape sequences and bracketed paste markers.
fn strip_ansi(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some(&'[') = chars.peek() {
                chars.next();
                while let Some(&nc) = chars.peek() {
                    if nc.is_ascii_alphabetic() || nc == '~' {
                        chars.next();
                        break;
                    }
                    chars.next();
                }
                continue;
            }
            chars.next();
            continue;
        }
        result.push(c);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_strips_ansi() {
        let input = "\x1b[31mERROR\x1b[0m: something";
        let result = ConnectionLearner::normalize(input);
        assert!(!result.contains("\x1b"));
        assert!(result.contains("error"));
    }

    #[test]
    fn test_normalize_strips_timestamps() {
        let input = "[    0.000000] Booting Linux on CPU 0";
        let result = ConnectionLearner::normalize(input);
        assert!(!result.contains("[    0.000000]"));
        assert!(result.contains("booting linux"));
    }

    #[test]
    fn test_normalize_compresses_whitespace() {
        let input = "hello    world  \t  foo";
        let result = ConnectionLearner::normalize(input);
        assert_eq!(result, "hello world foo");
    }

    #[test]
    fn test_normalize_lowercases() {
        let input = "U-Boot SPL 2024.01";
        let result = ConnectionLearner::normalize(input);
        assert_eq!(result, "u-boot spl 2024.01");
    }

    #[test]
    fn test_extract_first_n_lines() {
        let text = "line1\nline2\nline3\nline4\nline5\n";
        let result = ConnectionLearner::extract_first_n_lines(text, 3);
        assert_eq!(result.lines().count(), 3);
        assert!(result.starts_with("line1"));
    }

    #[test]
    fn test_compute_similarity_identical() {
        let text = "u-boot spl 2024.01 ddr version 1.08";
        let score = compute_similarity(text, text);
        assert!((score - 1.0).abs() < 0.01, "expected ~1.0, got {score}");
    }

    #[test]
    fn test_compute_similarity_different() {
        let a = "u-boot spl 2024.01 ddr version 1.08";
        let b = "linux version 6.1.0 starting kernel";
        let score = compute_similarity(a, b);
        assert!(score < 0.5, "expected <0.5, got {score}");
    }

    #[test]
    fn test_compute_similarity_empty() {
        assert_eq!(compute_similarity("", ""), 1.0);
        assert_eq!(compute_similarity("hello", ""), 0.0);
        assert_eq!(compute_similarity("", "world"), 0.0);
    }

    #[test]
    fn test_jaccard_3gram() {
        let a = "hello world";
        let b = "hello world";
        assert!((jaccard_3gram(a, b) - 1.0).abs() < 0.01);

        let c = "completely different";
        let score = jaccard_3gram(a, c);
        assert!(score < 0.3, "expected low similarity, got {score}");
    }

    #[test]
    fn test_normalize_strips_nul() {
        let input = "hello\0world\0";
        let result = ConnectionLearner::normalize(input);
        assert!(!result.contains('\0'));
        assert_eq!(result, "helloworld");
    }

    #[test]
    fn test_learn_config_defaults() {
        let cfg = LearnConfig::default();
        assert_eq!(cfg.cycles, 3);
        assert_eq!(cfg.compare_lines, 50);
        assert!((cfg.similarity_threshold - 0.93).abs() < 0.001);
        assert!((cfg.relay_broken_threshold - 0.10).abs() < 0.001);
    }

    #[test]
    fn test_learn_result_no_cycles() {
        let learner = ConnectionLearner::new(LearnConfig::default());
        let result = learner.evaluate_cycles(vec![], LearnMethod::HardwareReset);
        assert!(!result.connected);
        assert!(result.error.is_some());
    }

    #[test]
    fn test_learn_result_high_similarity() {
        let learner = ConnectionLearner::new(LearnConfig::default());
        let text = "u-boot spl 2024.01\nddr version 1.08\nbl31: v2.10\n";
        let normalized = ConnectionLearner::normalize(text);

        let cycles: Vec<LearnCycle> = (0..3)
            .map(|_| LearnCycle {
                log_path: PathBuf::from("/tmp/test.log"),
                first_50: normalized.clone(),
                full_content: text.to_string(),
                method: LearnMethod::HardwareReset,
            })
            .collect();

        let result = learner.evaluate_cycles(cycles, LearnMethod::HardwareReset);
        assert!(result.connected);
        assert!(result.best_similarity > 0.93);
    }

    #[test]
    fn test_learn_result_low_similarity() {
        let learner = ConnectionLearner::new(LearnConfig::default());

        let cycles = vec![
            LearnCycle {
                log_path: PathBuf::from("/tmp/test1.log"),
                first_50: ConnectionLearner::normalize("completely random garbage text"),
                full_content: String::new(),
                method: LearnMethod::HardwareReset,
            },
            LearnCycle {
                log_path: PathBuf::from("/tmp/test2.log"),
                first_50: ConnectionLearner::normalize("totally different unrelated output"),
                full_content: String::new(),
                method: LearnMethod::HardwareReset,
            },
            LearnCycle {
                log_path: PathBuf::from("/tmp/test3.log"),
                first_50: ConnectionLearner::normalize("yet another distinct sequence here"),
                full_content: String::new(),
                method: LearnMethod::HardwareReset,
            },
        ];

        let result = learner.evaluate_cycles(cycles, LearnMethod::HardwareReset);
        assert!(!result.connected);
        assert!(result.best_similarity < 0.93);
    }

    #[test]
    fn test_relay_broken_detection() {
        let cfg = LearnConfig {
            relay_broken_threshold: 0.10,
            similarity_threshold: 0.93,
            ..Default::default()
        };
        let learner = ConnectionLearner::new(cfg);

        let cycles = vec![
            LearnCycle {
                log_path: PathBuf::from("/tmp/t1.log"),
                first_50: ConnectionLearner::normalize("aaaaaaaaaa"),
                full_content: String::new(),
                method: LearnMethod::HardwareReset,
            },
            LearnCycle {
                log_path: PathBuf::from("/tmp/t2.log"),
                first_50: ConnectionLearner::normalize("bbbbbbbbbb"),
                full_content: String::new(),
                method: LearnMethod::HardwareReset,
            },
        ];

        let result = learner.evaluate_cycles(cycles, LearnMethod::HardwareReset);
        // Similarity very low → relay is broken
        assert!(!result.relay_verified);
        assert!(!result.connected);
    }
}
