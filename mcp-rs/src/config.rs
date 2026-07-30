//! Config loading — TOML (.target.toml).
//! Searches upward from CWD. TARGET_CONF env var overrides path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Default config values (same keys for both TOML and shell format)
pub fn defaults() -> HashMap<String, String> {
    HashMap::from([
        ("DEV_HOST_IP".into(), String::new()),
        ("DEV_HOST_USER".into(), String::new()),
        ("DEV_HOST_PASS".into(), String::new()),
        ("SERIAL_PORT".into(), "2000".into()),
        ("SERIAL_IP".into(), String::new()),
        ("SERIAL_PROTOCOL".into(), "raw".into()),
        ("SERIAL_BAUDRATE".into(), "1500000".into()),
        ("LOGIN_USER".into(), "root".into()),
        ("LOGIN_PASS".into(), String::new()),
        ("LOGIN_PROMPT".into(), String::new()),
        ("RELAY_PORT".into(), "0".into()),
        ("RELAY_IP".into(), String::new()),
        ("RESET_CHANNEL".into(), "0".into()),
        ("MASKROM_CHANNEL".into(), "0".into()),
        ("RECOVERY_CHANNEL".into(), "0".into()),
        ("DEV_CTL".into(), String::new()),
        ("HANG_TIMEOUT".into(), "60".into()),
        ("HANG_HYSTERESIS".into(), "3".into()),
        ("MAX_ARCHIVED_LOGS".into(), "10".into()),
        ("MAX_LOG_FILE_SIZE".into(), "100".into()),
        ("DUT_DIR".into(), ".dut-serial".into()),
        ("UBOOT_INTERRUPT_STRATEGY".into(), "lava".into()),
        ("UBOOT_INTERRUPT_CHAR".into(), "ctrl_c".into()),
        ("LOCK_DIR".into(), "/tmp/debug-console/locks".into()),
        ("REFERENCE_LOG".into(), String::new()),
        ("FLASH_LOADER_CMD".into(), String::new()),
        ("FLASH_LIST_DEVICES_CMD".into(), String::new()),
        ("RESET_TIME_MS".into(), String::new()),
        ("LEARNER_STAGE_THRESHOLD".into(), String::new()),
        ("LEARNER_CRASH_THRESHOLD".into(), String::new()),
    ])
}

/// Loaded configuration (flat key-value, compatible with both formats)
#[derive(Debug, Clone)]
pub struct Config {
    pub values: HashMap<String, String>,
    pub config_path: Option<PathBuf>,
    pub project_dir: Option<PathBuf>,
    #[allow(dead_code)]
    pub format: ConfigFormat,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigFormat {
    Toml,
    None,
}

impl Config {
    pub fn get(&self, key: &str) -> &str {
        self.values.get(key).map(|s| s.as_str()).unwrap_or("")
    }

    #[allow(dead_code)]
    pub fn get_int(&self, key: &str) -> i64 {
        self.get(key).parse().unwrap_or(0)
    }

    pub fn get_str_or(&self, key: &str, default: &str) -> String {
        let v = self.get(key);
        if v.is_empty() {
            default.to_string()
        } else {
            v.to_string()
        }
    }

    pub fn dev_host_ip(&self) -> String {
        self.get_str_or("DEV_HOST_IP", "")
    }
    #[allow(dead_code)]
    pub fn serial_ip(&self) -> String {
        let v = self.get("SERIAL_IP");
        if v.is_empty() {
            self.dev_host_ip()
        } else {
            v.to_string()
        }
    }
    pub fn serial_target(&self) -> String {
        let v = self.get("SERIAL_PORT");
        if v.is_empty() {
            return "2000".into();
        }
        if v.starts_with("/dev/") || v.starts_with("COM") {
            return v.to_string();
        }
        if v.parse::<u16>().is_ok() {
            return v.to_string();
        }
        "2000".into()
    }
    pub fn relay_port(&self) -> u16 {
        self.get("RELAY_PORT").parse().unwrap_or(0)
    }
    pub fn reset_channel(&self) -> u8 {
        self.get("RESET_CHANNEL").parse().unwrap_or(0)
    }
    pub fn maskrom_channel(&self) -> u8 {
        self.get("MASKROM_CHANNEL").parse().unwrap_or(0)
    }
    #[allow(dead_code)]
    pub fn recovery_channel(&self) -> u8 {
        self.get("RECOVERY_CHANNEL").parse().unwrap_or(0)
    }
    pub fn power_channel(&self) -> u8 {
        self.get("POWER_CHANNEL").parse().unwrap_or(0)
    }
    pub fn power_off_time_ms(&self) -> u64 {
        self.get("POWER_OFF_TIME_MS")
            .parse()
            .unwrap_or(3000)
            .max(3000)
    }
    #[allow(dead_code)]
    pub fn relay_ip(&self) -> String {
        let v = self.get("RELAY_IP");
        if v.is_empty() {
            self.dev_host_ip()
        } else {
            v.to_string()
        }
    }
    pub fn hang_timeout(&self) -> u64 {
        self.get("HANG_TIMEOUT").parse().unwrap_or(60)
    }
    pub fn hang_hysteresis(&self) -> u32 {
        self.get("HANG_HYSTERESIS").parse().unwrap_or(3)
    }
    pub fn max_archived_logs(&self) -> usize {
        self.get("MAX_ARCHIVED_LOGS").parse().unwrap_or(10)
    }
    pub fn max_log_file_size_mb(&self) -> u64 {
        self.get("MAX_LOG_FILE_SIZE").parse().unwrap_or(100)
    }
    pub fn dut_dir(&self) -> String {
        self.get_str_or("DUT_DIR", ".dut-serial")
    }
    pub fn uboot_interrupt_char(&self) -> u8 {
        let v = self.get("UBOOT_INTERRUPT_CHAR");
        match v {
            "ctrl_c" | "0x03" => 0x03,
            _ if v.len() == 1 => v.as_bytes()[0],
            _ => {
                if v.starts_with("0x") {
                    u8::from_str_radix(&v[2..], 16).unwrap_or(0x03)
                } else {
                    v.parse::<u8>().unwrap_or(0x03)
                }
            }
        }
    }
    pub fn lock_dir(&self) -> String {
        self.get_str_or("LOCK_DIR", "/tmp/debug-console/locks")
    }
    pub fn login_user(&self) -> String {
        self.get_str_or("LOGIN_USER", "root")
    }
    pub fn login_pass(&self) -> String {
        self.get("LOGIN_PASS").to_string()
    }
    /// Custom login prompt regex. Empty string → use default `(?:.*\s)?login:\s*$`.
    pub fn login_prompt(&self) -> String {
        self.get("LOGIN_PROMPT").to_string()
    }
    pub fn reference_log(&self) -> String {
        self.get("REFERENCE_LOG").to_string()
    }
    /// Minimum USB relay reset pulse time in ms. Returns 0 if not
    /// configured in `.target.toml` (→ use compile-time default, 3000).
    pub fn reset_time_ms(&self) -> u64 {
        self.get("RESET_TIME_MS").parse().unwrap_or(0)
    }
    /// StageLearner similarity threshold for normal boot stages (0.0–1.0).
    /// Falls back to Cargo.toml `[package.metadata.learn]` → 0.45.
    pub fn learner_stage_threshold(&self) -> f64 {
        let default: f64 = option_env!("LEARN_STAGE_THRESHOLD")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.45);
        self.get("LEARNER_STAGE_THRESHOLD")
            .parse()
            .unwrap_or(default)
    }
    /// StageLearner similarity threshold for crash detection (0.0–1.0).
    /// Falls back to Cargo.toml `[package.metadata.learn]` → 0.50.
    pub fn learner_crash_threshold(&self) -> f64 {
        let default: f64 = option_env!("LEARN_CRASH_THRESHOLD")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.50);
        self.get("LEARNER_CRASH_THRESHOLD")
            .parse()
            .unwrap_or(default)
    }
    /// Custom crash patterns for boot log scanning. Each entry is a substring
    /// (not regex). Falls back to Cargo.toml `[package.metadata.learn]`.
    pub fn crash_patterns(&self) -> Vec<String> {
        let val = self.get("CRASH_PATTERNS");
        if !val.is_empty() {
            val.split('\n')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else {
            option_env!("LEARN_CRASH_PATTERNS")
                .unwrap_or("Kernel panic\nBUG:\nOops:\nUnable to handle kernel\nSegmentation fault\n---[ end trace\npanic - not syncing")
                .split('\n')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }
    }

    #[allow(dead_code)]
    pub fn dev_ctl(&self) -> String {
        self.get("DEV_CTL").to_string()
    }
    #[allow(dead_code)]
    pub fn dut_aliases(&self) -> Vec<String> {
        self.get("DUT_ALIASES")
            .split(',')
            .filter_map(|s| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            })
            .collect()
    }
}

// ── Multi-DUT support ─────────────────────────────────────────────────

/// Dev host configuration extracted from a `[[dev_hosts]]` entry.
#[derive(Debug, Clone)]
pub struct DevHostConfig {
    pub alias: String,
    pub ip: String,
    pub user: String,
    pub pass: String,
}

/// Per-DUT configuration extracted from a `[[dut]]` entry.
#[derive(Debug, Clone)]
pub struct DutConfig {
    pub alias: String,
    pub dev_host_alias: String,
    pub dev_host_ip: String,
    pub dev_host_user: String,
    pub dev_host_pass: String,
    pub serial_port: String,
    pub serial_ip: String,
    pub relay_ip: String,
    pub relay_type: String,
    pub relay_port: u16,
    pub reset_ch: u8,
    pub maskrom_ch: u8,
    pub recovery_ch: u8,
    pub power_ch: u8,
    pub power_off_time_ms: u64,
    pub reference_log: String,
    pub dut_dir: String,
    pub login_user: String,
    pub login_pass: String,
    pub learner_stage_threshold: f64,
    pub learner_crash_threshold: f64,
    pub crash_patterns: Vec<String>,
}

impl DutConfig {
    /// Build a `Config` map from this DUT config, inheriting from the global config.
    pub fn to_config_map(&self, global: &HashMap<String, String>) -> HashMap<String, String> {
        let mut cfg = global.clone();
        // Override with DUT-specific values
        cfg.insert("DUT_ALIAS".into(), self.alias.clone());
        if !self.dev_host_ip.is_empty() {
            cfg.insert("DEV_HOST_IP".into(), self.dev_host_ip.clone());
        }
        if !self.dev_host_user.is_empty() {
            cfg.insert("DEV_HOST_USER".into(), self.dev_host_user.clone());
        }
        if !self.dev_host_pass.is_empty() {
            cfg.insert("DEV_HOST_PASS".into(), self.dev_host_pass.clone());
        }
        if !self.serial_port.is_empty() {
            cfg.insert("SERIAL_PORT".into(), self.serial_port.clone());
        }
        if !self.serial_ip.is_empty() {
            cfg.insert("SERIAL_IP".into(), self.serial_ip.clone());
        }
        if !self.relay_ip.is_empty() {
            cfg.insert("RELAY_IP".into(), self.relay_ip.clone());
        }
        if !self.relay_type.is_empty() {
            cfg.insert("RELAY_TYPE".into(), self.relay_type.clone());
        }
        if self.relay_port > 0 {
            cfg.insert("RELAY_PORT".into(), self.relay_port.to_string());
        }
        if self.reset_ch > 0 {
            cfg.insert("RESET_CHANNEL".into(), self.reset_ch.to_string());
        }
        if self.maskrom_ch > 0 {
            cfg.insert("MASKROM_CHANNEL".into(), self.maskrom_ch.to_string());
        }
        if self.recovery_ch > 0 {
            cfg.insert("RECOVERY_CHANNEL".into(), self.recovery_ch.to_string());
        }
        if self.power_ch > 0 {
            cfg.insert("POWER_CHANNEL".into(), self.power_ch.to_string());
        }
        if self.power_off_time_ms > 0 {
            cfg.insert(
                "POWER_OFF_TIME_MS".into(),
                self.power_off_time_ms.to_string(),
            );
        }
        if !self.reference_log.is_empty() {
            cfg.insert("REFERENCE_LOG".into(), self.reference_log.clone());
        }
        if !self.dut_dir.is_empty() {
            cfg.insert("DUT_DIR".into(), self.dut_dir.clone());
        }
        if !self.login_user.is_empty() {
            cfg.insert("LOGIN_USER".into(), self.login_user.clone());
        }
        if !self.login_pass.is_empty() {
            cfg.insert("LOGIN_PASS".into(), self.login_pass.clone());
        }
        if self.learner_stage_threshold > 0.0 {
            cfg.insert(
                "LEARNER_STAGE_THRESHOLD".into(),
                self.learner_stage_threshold.to_string(),
            );
        }
        if self.learner_crash_threshold > 0.0 {
            cfg.insert(
                "LEARNER_CRASH_THRESHOLD".into(),
                self.learner_crash_threshold.to_string(),
            );
        }
        if !self.crash_patterns.is_empty() {
            cfg.insert("CRASH_PATTERNS".into(), self.crash_patterns.join("\n"));
        }
        cfg
    }
}

/// Auto-generate `.mcp.json` in `project_dir` if `.target.toml` exists but `.mcp.json` does not.
///
/// Agent constraint: generates MCP config, never touches `.target.toml`.
/// Called by SessionStart hook or first-time setup.
pub fn ensure_mcp_json(project_dir: &Path) -> Result<bool, String> {
    let target_toml = project_dir.join(".target.toml");
    let mcp_json = project_dir.join(".mcp.json");

    if !target_toml.exists() {
        return Ok(false); // No .target.toml → nothing to do
    }
    if mcp_json.exists() {
        return Ok(false); // Already exists
    }

    let config = serde_json::json!({
        "mcpServers": {
            "debug-console": {
                "command": "debug-console-mcp",
                "args": ["--http"],
                "env": {
                    "TARGET_CONF": target_toml.to_string_lossy()
                },
                "description": "Serial debug MCP for embedded Linux target boards"
            }
        }
    });

    let content = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("JSON serialization error: {e}"))?;

    std::fs::write(&mcp_json, content).map_err(|e| format!("Cannot write .mcp.json: {e}"))?;

    tracing::info!("Auto-generated .mcp.json at {}", mcp_json.display());
    Ok(true)
}

/// Parse all DUT entries from a TOML file, returning a list of `DutConfig`.
/// If no `[[dut]]` entries exist, returns a single default DUT with "default" alias.
pub fn parse_dut_configs(toml_path: &Path) -> Result<Vec<DutConfig>, String> {
    let content = std::fs::read_to_string(toml_path)
        .map_err(|e| format!("Cannot read {toml_path:?}: {e}"))?;
    let t: TargetToml = toml::from_str(&content).map_err(|e| format!("TOML parse error: {e}"))?;

    let global = parse_toml_config(toml_path);

    // Build dev_host registry from [[dev_hosts]] array (or legacy [dev_host])
    let mut dev_hosts: Vec<DevHostConfig> = Vec::new();
    let dev_host_list: Vec<&DevHostToml> = t.dev_hosts
        .as_ref()
        .map(|x| x.as_slice().iter().collect())
        .unwrap_or_default();
    for dh in &dev_host_list {
        dev_hosts.push(DevHostConfig {
            alias: dh.alias.clone().unwrap_or_default(),
            ip: dh.ip.clone().unwrap_or_default(),
            user: dh.user.clone().unwrap_or_default(),
            pass: dh.pass.clone().unwrap_or_default(),
        });
    }

    if let Some(duts) = t.dut {
        if duts.is_empty() {
            return Ok(vec![default_dut_config(&global, &dev_hosts)]);
        }
        let configs: Vec<DutConfig> = duts
            .into_iter()
            .map(|d| dut_toml_to_config(d, &global, &dev_hosts))
            .collect();

        // Check for port conflicts between DUTs
        let mut ports_seen: std::collections::HashMap<String, Vec<String>> = HashMap::new();
        for dut in &configs {
            let key = format!("{}:{}", dut.dev_host_ip, dut.serial_port);
            ports_seen.entry(key).or_default().push(dut.alias.clone());
        }
        for (key, aliases) in &ports_seen {
            if aliases.len() > 1 {
                tracing::warn!(
                    "[AGENT-NOTIFY] Port conflict: {} shared by DUTs: {:?}. \
                     Each DUT needs a unique ser2net port.",
                    key,
                    aliases
                );
            }
        }

        Ok(configs)
    } else {
        Ok(vec![default_dut_config(&global, &dev_hosts)])
    }
}

fn resolve_dev_host(dev_hosts: &[DevHostConfig], alias: Option<&str>) -> DevHostConfig {
    if let Some(ref_alias) = alias {
        for dh in dev_hosts {
            if dh.alias == ref_alias {
                return dh.clone();
            }
        }
    }
    // Fallback: first dev_host, or empty
    dev_hosts.first().cloned().unwrap_or(DevHostConfig {
        alias: "default".into(),
        ip: String::new(),
        user: String::new(),
        pass: String::new(),
    })
}

fn default_dut_config(global: &HashMap<String, String>, dev_hosts: &[DevHostConfig]) -> DutConfig {
    let alias = global
        .get("DUT_ALIAS")
        .cloned()
        .unwrap_or_else(|| "default".into());
    let dut_dir = global
        .get("DUT_DIR")
        .cloned()
        .unwrap_or_else(|| ".dut-serial".into());
    let dh = resolve_dev_host(dev_hosts, None);
    let dev_host_ip = if dh.ip.is_empty() {
        global.get("DEV_HOST_IP").cloned().unwrap_or_default()
    } else {
        dh.ip
    };
    let dev_host_user = if dh.user.is_empty() {
        global.get("DEV_HOST_USER").cloned().unwrap_or_default()
    } else {
        dh.user
    };
    let dev_host_pass = if dh.pass.is_empty() {
        global.get("DEV_HOST_PASS").cloned().unwrap_or_default()
    } else {
        dh.pass
    };
    DutConfig {
        alias,
        dev_host_alias: dh.alias,
        dev_host_ip,
        dev_host_user,
        dev_host_pass,
        serial_port: global.get("SERIAL_PORT").cloned().unwrap_or_default(),
        serial_ip: global.get("SERIAL_IP").cloned().unwrap_or_default(),
        relay_ip: global.get("RELAY_IP").cloned().unwrap_or_default(),
        relay_type: global
            .get("RELAY_TYPE")
            .cloned()
            .unwrap_or_else(|| "usb-relay".into()),
        relay_port: global
            .get("RELAY_PORT")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        reset_ch: global
            .get("RESET_CHANNEL")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        maskrom_ch: global
            .get("MASKROM_CHANNEL")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        recovery_ch: global
            .get("RECOVERY_CHANNEL")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        power_ch: global
            .get("POWER_CHANNEL")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        power_off_time_ms: global
            .get("POWER_OFF_TIME_MS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(3000)
            .max(3000),
        reference_log: global.get("REFERENCE_LOG").cloned().unwrap_or_default(),
        dut_dir,
        login_user: global.get("LOGIN_USER").cloned().unwrap_or_default(),
        login_pass: global.get("LOGIN_PASS").cloned().unwrap_or_default(),
        learner_stage_threshold: 0.0,
        learner_crash_threshold: 0.0,
        crash_patterns: Vec::new(),
    }
}

fn dut_toml_to_config(
    dut: DutToml,
    global: &HashMap<String, String>,
    dev_hosts: &[DevHostConfig],
) -> DutConfig {
    let alias = dut.alias.unwrap_or_else(|| "unnamed".into());
    let dut_dir = dut
        .dut_dir
        .unwrap_or_else(|| format!(".dut-serial/{alias}"));

    // Resolve dev_host
    let dh = resolve_dev_host(dev_hosts, dut.dev_host.as_deref());

    let mut serial_port = String::new();
    let mut serial_ip = String::new();
    if let Some(ref s) = dut.serial {
        serial_port = s
            .port
            .as_ref()
            .map(|v| toml_value_to_string(v))
            .unwrap_or_default();
        serial_ip = s.ip.clone().unwrap_or_default();
    }
    if serial_port.is_empty() {
        serial_port = global.get("SERIAL_PORT").cloned().unwrap_or_default();
    }
    if serial_ip.is_empty() {
        serial_ip = dh.ip.clone(); // fall back to dev host IP
    }

    let mut relay_port: u16 = 0;
    let mut relay_ip = String::new();
    let mut reset_ch: u8 = 0;
    let mut maskrom_ch: u8 = 0;
    let mut recovery_ch: u8 = 0;
    let mut power_ch: u8 = 0;
    let mut power_off_time_ms: u64 = 3000;
    if let Some(ref r) = dut.relay {
        relay_port = r.port.unwrap_or(0);
        relay_ip = r.ip.clone().unwrap_or_default();
        reset_ch = r.reset_ch.or(r.reset_channel).unwrap_or(0);
        maskrom_ch = r.maskrom_ch.or(r.maskrom_channel).unwrap_or(0);
        recovery_ch = r.recovery_ch.or(r.recovery_channel).unwrap_or(0);
        power_ch = r.power_ch.or(r.power_channel).unwrap_or(0);
        power_off_time_ms = r.power_off_time_ms.unwrap_or(3000).max(3000);
    }
    if relay_port == 0 {
        relay_port = global
            .get("RELAY_PORT")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
    }
    if relay_ip.is_empty() {
        relay_ip = global.get("RELAY_IP").cloned().unwrap_or_default();
    }
    if relay_ip.is_empty() {
        relay_ip = dh.ip.clone(); // fall back to dev host IP
    }
    if reset_ch == 0 {
        reset_ch = global
            .get("RESET_CHANNEL")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
    }
    if power_ch == 0 {
        power_ch = global
            .get("POWER_CHANNEL")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
    }
    if power_off_time_ms < 3000 {
        power_off_time_ms = global
            .get("POWER_OFF_TIME_MS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(3000)
            .max(3000);
    }

    let mut learner_stage_threshold = 0.0f64;
    let mut learner_crash_threshold = 0.0f64;
    let mut crash_patterns: Vec<String> = Vec::new();

    let mut reference_log = String::new();
    if let Some(ref m) = dut.monitor {
        reference_log = m.reference_log.clone().unwrap_or_default();
        learner_stage_threshold = m.learner_stage_threshold.unwrap_or(0.0);
        learner_crash_threshold = m.learner_crash_threshold.unwrap_or(0.0);
        crash_patterns = m.crash_patterns.clone().unwrap_or_default();
    }
    if reference_log.is_empty() {
        reference_log = dut
            .reference_log
            .unwrap_or_else(|| format!("{dut_dir}/reference-boot.log"));
    }
    if reference_log.is_empty() {
        reference_log = global.get("REFERENCE_LOG").cloned().unwrap_or_default();
    }

    let login_user = dut
        .target
        .as_ref()
        .and_then(|t| t.login_user.clone())
        .unwrap_or_else(|| global.get("LOGIN_USER").cloned().unwrap_or_default());
    let login_pass = dut
        .target
        .as_ref()
        .and_then(|t| t.login_pass.clone())
        .unwrap_or_else(|| global.get("LOGIN_PASS").cloned().unwrap_or_default());

    DutConfig {
        alias,
        dev_host_alias: dh.alias,
        dev_host_ip: dh.ip,
        dev_host_user: dh.user,
        dev_host_pass: dh.pass,
        relay_type: dut
            .relay
            .as_ref()
            .and_then(|r| r.relay_type.clone())
            .unwrap_or_else(|| "usb-relay".into()),
        serial_port,
        serial_ip,
        relay_ip,
        relay_port,
        reset_ch,
        maskrom_ch,
        recovery_ch,
        power_ch,
        power_off_time_ms,
        reference_log,
        dut_dir,
        login_user,
        login_pass,
        learner_stage_threshold,
        learner_crash_threshold,
        crash_patterns,
    }
}

// ── TOML parsing ──────────────────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
struct TargetToml {
    // [[dev_hosts]] array only.
    #[serde(default)]
    dev_hosts: Option<Vec<DevHostToml>>,
    serial: Option<SerialToml>,
    target: Option<TargetCredsToml>,
    uboot: Option<UbootToml>,
    relay: Option<RelayToml>,
    monitor: Option<MonitorToml>,
    flash: Option<FlashToml>,
    control: Option<ControlToml>,
    dut: Option<Vec<DutToml>>,
    // top-level keys for backward compat
    reference_log: Option<String>,
    lock_dir: Option<String>,
    dut_dir: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct DevHostToml {
    alias: Option<String>,
    ip: Option<String>,
    user: Option<String>,
    pass: Option<String>,
}


#[derive(serde::Deserialize, Default)]
struct SerialToml {
    ip: Option<String>,
    port: Option<toml::Value>, // can be int or string
    baudrate: Option<toml::Value>,
}

#[derive(serde::Deserialize, Default)]
struct TargetCredsToml {
    login_user: Option<String>,
    login_pass: Option<String>,
    /// Custom login prompt regex. If not set, defaults to:
    /// `(?m)(?:.*\\s)?login:\\s*$`
    login_prompt: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct UbootToml {
    interrupt_char: Option<String>,
    interrupt_strategy: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct RelayToml {
    #[serde(rename = "type")]
    relay_type: Option<String>,
    port: Option<u16>,
    ip: Option<String>,
    reset_ch: Option<u8>,
    maskrom_ch: Option<u8>,
    recovery_ch: Option<u8>,
    power_ch: Option<u8>,
    reset_channel: Option<u8>,
    maskrom_channel: Option<u8>,
    recovery_channel: Option<u8>,
    power_channel: Option<u8>,
    /// Minimum USB relay reset pulse time in ms. Overrides Cargo.toml
    /// default (3000). 0 or missing → use compile-time default.
    reset_time_ms: Option<u64>,
    /// Minimum power-off duration in ms. Clamped to >= 3000.
    power_off_time_ms: Option<u64>,
}

#[derive(serde::Deserialize, Default)]
struct MonitorToml {
    hang_timeout: Option<u64>,
    hang_hysteresis: Option<u32>,
    max_archived_logs: Option<usize>,
    max_log_file_size: Option<u64>,
    reference_log: Option<String>,
    /// StageLearner similarity threshold for normal boot stages (0.0–1.0).
    /// Default: 0.45 (from Cargo.toml [package.metadata.learn]).
    learner_stage_threshold: Option<f64>,
    /// StageLearner similarity threshold for crash detection (0.0–1.0).
    /// Default: 0.50 (from Cargo.toml [package.metadata.learn]).
    learner_crash_threshold: Option<f64>,
    /// Custom crash patterns for boot log scanning. Each string is matched as
    /// a substring (not regex). Defaults from Cargo.toml [package.metadata.learn].
    #[serde(default)]
    crash_patterns: Option<Vec<String>>,
}

#[derive(serde::Deserialize, Default)]
struct FlashToml {
    tool: Option<String>,
    full_image_cmd: Option<String>,
    kernel_image_cmd: Option<String>,
    loader_bin: Option<String>,
    loader_cmd: Option<String>,
    list_devices_cmd: Option<String>,
    upload_dir: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct ControlToml {
    dev_ctl: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct DutToml {
    alias: Option<String>,
    /// Reference to a `[[dev_hosts]]` alias. If unset, uses the first dev_host.
    dev_host: Option<String>,
    serial: Option<SerialToml>,
    target: Option<TargetCredsToml>,
    relay: Option<RelayToml>,
    monitor: Option<MonitorToml>,
    flash: Option<FlashToml>,
    control: Option<ControlToml>,
    reference_log: Option<String>,
    dut_dir: Option<String>,
}

fn toml_value_to_string(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        _ => v.to_string(),
    }
}

fn parse_toml_config(path: &Path) -> HashMap<String, String> {
    let mut cfg = HashMap::new();
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Cannot read TOML config {:?}: {e}", path);
            return cfg;
        }
    };
    let t: TargetToml = match toml::from_str(&content) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("TOML parse error in {:?}: {e}", path);
            return cfg;
        }
    };

    // Populate global config from [[dev_hosts]] (or legacy [dev_host])
    let dev_host = t.dev_hosts.as_ref().and_then(|x| x.first());
    if let Some(dh) = dev_host {
        if let Some(ref v) = dh.alias {
            cfg.insert("DEV_HOST_ALIAS".into(), v.clone());
        }
        if let Some(ref v) = dh.ip {
            cfg.insert("DEV_HOST_IP".into(), v.clone());
        }
        if let Some(ref v) = dh.user {
            cfg.insert("DEV_HOST_USER".into(), v.clone());
        }
        if let Some(ref v) = dh.pass {
            cfg.insert("DEV_HOST_PASS".into(), v.clone());
        }
    }
    // serial
    if let Some(s) = t.serial {
        if let Some(v) = s.ip {
            cfg.insert("SERIAL_IP".into(), v);
        }
        if let Some(v) = s.port {
            cfg.insert("SERIAL_PORT".into(), toml_value_to_string(&v));
        }
        if let Some(v) = s.baudrate {
            cfg.insert("SERIAL_BAUDRATE".into(), toml_value_to_string(&v));
        }
    }
    // target
    if let Some(tg) = t.target {
        if let Some(v) = tg.login_user {
            cfg.insert("LOGIN_USER".into(), v);
        }
        if let Some(v) = tg.login_pass {
            cfg.insert("LOGIN_PASS".into(), v);
        }
        if let Some(v) = tg.login_prompt {
            cfg.insert("LOGIN_PROMPT".into(), v);
        }
    }
    // uboot
    if let Some(ub) = t.uboot {
        if let Some(v) = ub.interrupt_char {
            cfg.insert("UBOOT_INTERRUPT_CHAR".into(), v);
        }
        if let Some(v) = ub.interrupt_strategy {
            cfg.insert("UBOOT_INTERRUPT_STRATEGY".into(), v);
        }
    }
    // relay
    if let Some(r) = t.relay {
        if let Some(v) = r.relay_type {
            cfg.insert("RELAY_TYPE".into(), v);
        }
        if let Some(v) = r.port {
            cfg.insert("RELAY_PORT".into(), v.to_string());
        }
        if let Some(v) = r.ip {
            cfg.insert("RELAY_IP".into(), v);
        }
        if let Some(v) = r.reset_ch.or(r.reset_channel) {
            cfg.insert("RESET_CHANNEL".into(), v.to_string());
        }
        if let Some(v) = r.maskrom_ch.or(r.maskrom_channel) {
            cfg.insert("MASKROM_CHANNEL".into(), v.to_string());
        }
        if let Some(v) = r.recovery_ch.or(r.recovery_channel) {
            cfg.insert("RECOVERY_CHANNEL".into(), v.to_string());
        }
        if let Some(v) = r.reset_time_ms {
            cfg.insert("RESET_TIME_MS".into(), v.to_string());
        }
    }
    // monitor
    if let Some(m) = t.monitor {
        if let Some(v) = m.hang_timeout {
            cfg.insert("HANG_TIMEOUT".into(), v.to_string());
        }
        if let Some(v) = m.hang_hysteresis {
            cfg.insert("HANG_HYSTERESIS".into(), v.to_string());
        }
        if let Some(v) = m.max_archived_logs {
            cfg.insert("MAX_ARCHIVED_LOGS".into(), v.to_string());
        }
        if let Some(v) = m.max_log_file_size {
            cfg.insert("MAX_LOG_FILE_SIZE".into(), v.to_string());
        }
        if let Some(v) = m.reference_log {
            cfg.insert("REFERENCE_LOG".into(), v);
        }
    }
    // flash
    if let Some(f) = t.flash {
        if let Some(v) = f.tool {
            cfg.insert("FLASH_TOOL".into(), v);
        }
        if let Some(v) = f.full_image_cmd {
            cfg.insert("FLASH_FULL_IMAGE_CMD".into(), v);
        }
        if let Some(v) = f.kernel_image_cmd {
            cfg.insert("FLASH_KERNEL_IMAGE_CMD".into(), v);
        }
        if let Some(v) = f.loader_bin {
            cfg.insert("FLASH_LOADER_BIN".into(), v);
        }
        if let Some(v) = f.loader_cmd {
            cfg.insert("FLASH_LOADER_CMD".into(), v);
        }
        if let Some(v) = f.list_devices_cmd {
            cfg.insert("FLASH_LIST_DEVICES_CMD".into(), v);
        }
        if let Some(v) = f.upload_dir {
            cfg.insert("FLASH_UPLOAD_DIR".into(), v);
        }
    }
    if let Some(c) = t.control {
        if let Some(v) = c.dev_ctl {
            cfg.insert("DEV_CTL".into(), v);
        }
    }
    // top-level
    if let Some(v) = t.reference_log {
        cfg.insert("REFERENCE_LOG".into(), v);
    }
    if let Some(v) = t.lock_dir {
        cfg.insert("LOCK_DIR".into(), v);
    }
    if let Some(v) = t.dut_dir {
        cfg.insert("DUT_DIR".into(), v);
    }
    if let Some(duts) = t.dut {
        let aliases = duts
            .iter()
            .filter_map(|d| d.alias.clone())
            .collect::<Vec<_>>();
        if !aliases.is_empty() {
            cfg.insert("DUT_ALIASES".into(), aliases.join(","));
        }
        if !cfg.contains_key("DUT_ALIAS") {
            if let Some(first) = duts.into_iter().next() {
                merge_dut_config(&mut cfg, first);
            }
        }
    }

    cfg
}

fn merge_dut_config(cfg: &mut HashMap<String, String>, dut: DutToml) {
    if let Some(ref alias) = dut.alias {
        cfg.insert("DUT_ALIAS".into(), alias.clone());
        // Auto-derive dut_dir from alias if not explicitly set
        if dut.dut_dir.is_none() {
            cfg.insert("DUT_DIR".into(), format!(".dut-serial/{alias}"));
        }
    }
    if let Some(s) = dut.serial {
        if let Some(v) = s.ip {
            cfg.insert("SERIAL_IP".into(), v);
        }
        if let Some(v) = s.port {
            cfg.insert("SERIAL_PORT".into(), toml_value_to_string(&v));
        }
        if let Some(v) = s.baudrate {
            cfg.insert("SERIAL_BAUDRATE".into(), toml_value_to_string(&v));
        }
    }
    if let Some(tg) = dut.target {
        if let Some(v) = tg.login_user {
            cfg.insert("LOGIN_USER".into(), v);
        }
        if let Some(v) = tg.login_pass {
            cfg.insert("LOGIN_PASS".into(), v);
        }
        if let Some(v) = tg.login_prompt {
            cfg.insert("LOGIN_PROMPT".into(), v);
        }
    }
    if let Some(r) = dut.relay {
        if let Some(v) = r.relay_type {
            cfg.insert("RELAY_TYPE".into(), v);
        }
        if let Some(v) = r.port {
            cfg.insert("RELAY_PORT".into(), v.to_string());
        }
        if let Some(v) = r.ip {
            cfg.insert("RELAY_IP".into(), v);
        }
        if let Some(v) = r.reset_ch.or(r.reset_channel) {
            cfg.insert("RESET_CHANNEL".into(), v.to_string());
        }
        if let Some(v) = r.maskrom_ch.or(r.maskrom_channel) {
            cfg.insert("MASKROM_CHANNEL".into(), v.to_string());
        }
        if let Some(v) = r.recovery_ch.or(r.recovery_channel) {
            cfg.insert("RECOVERY_CHANNEL".into(), v.to_string());
        }
        if let Some(v) = r.reset_time_ms {
            cfg.insert("RESET_TIME_MS".into(), v.to_string());
        }
    }
    if let Some(m) = dut.monitor {
        if let Some(v) = m.hang_timeout {
            cfg.insert("HANG_TIMEOUT".into(), v.to_string());
        }
        if let Some(v) = m.hang_hysteresis {
            cfg.insert("HANG_HYSTERESIS".into(), v.to_string());
        }
        if let Some(v) = m.max_archived_logs {
            cfg.insert("MAX_ARCHIVED_LOGS".into(), v.to_string());
        }
        if let Some(v) = m.max_log_file_size {
            cfg.insert("MAX_LOG_FILE_SIZE".into(), v.to_string());
        }
        if let Some(v) = m.reference_log {
            cfg.insert("REFERENCE_LOG".into(), v);
        }
    }
    if let Some(f) = dut.flash {
        if let Some(v) = f.tool {
            cfg.insert("FLASH_TOOL".into(), v);
        }
        if let Some(v) = f.full_image_cmd {
            cfg.insert("FLASH_FULL_IMAGE_CMD".into(), v);
        }
        if let Some(v) = f.kernel_image_cmd {
            cfg.insert("FLASH_KERNEL_IMAGE_CMD".into(), v);
        }
        if let Some(v) = f.loader_bin {
            cfg.insert("FLASH_LOADER_BIN".into(), v);
        }
        if let Some(v) = f.loader_cmd {
            cfg.insert("FLASH_LOADER_CMD".into(), v);
        }
        if let Some(v) = f.list_devices_cmd {
            cfg.insert("FLASH_LIST_DEVICES_CMD".into(), v);
        }
        if let Some(v) = f.upload_dir {
            cfg.insert("FLASH_UPLOAD_DIR".into(), v);
        }
    }
    if let Some(c) = dut.control {
        if let Some(v) = c.dev_ctl {
            cfg.insert("DEV_CTL".into(), v);
        }
    }
    if let Some(v) = dut.reference_log {
        cfg.insert("REFERENCE_LOG".into(), v);
    }
    if let Some(v) = dut.dut_dir {
        cfg.insert("DUT_DIR".into(), v);
    }
}

// ── Config path discovery ─────────────────────────────────────────────

/// Find config file in CWD: searches for .target.toml.
/// TARGET_CONF env var overrides the search path.
fn find_config_path() -> Option<PathBuf> {
    // TARGET_CONF env var overrides
    if let Ok(env) = std::env::var("TARGET_CONF") {
        let p = PathBuf::from(&env);
        if p.exists() {
            return Some(p);
        }
    }
    let mut d = std::env::current_dir().ok()?;
    loop {
        for name in &[".target.toml"] {
            let candidate = d.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        if !d.pop() {
            break;
        }
    }
    None
}

/// Load config: defaults + .target.toml overrides.
pub fn load_config() -> Config {
    let mut values = defaults();
    let path = find_config_path();

    let format = match path {
        Some(ref p) if p.extension().is_some_and(|e| e == "toml") => ConfigFormat::Toml,
        _ => ConfigFormat::None,
    };

    if let Some(ref p) = path {
        let file_cfg = parse_toml_config(p);
        values.extend(file_cfg);
        if let Ok(alias) = std::env::var("TARGET_DUT_ALIAS") {
            if !alias.trim().is_empty() {
                match parse_dut_configs(p) {
                    Ok(duts) => {
                        if let Some(dut) = duts.into_iter().find(|d| d.alias == alias) {
                            values = dut.to_config_map(&values);
                        } else {
                            tracing::warn!(
                                "TARGET_DUT_ALIAS={} not found in {}; using default DUT",
                                alias,
                                p.display()
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Cannot parse DUT configs for TARGET_DUT_ALIAS: {e}")
                    }
                }
            }
        }
    }

    let config_path = path;
    let project_dir = config_path
        .as_ref()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    Config {
        values,
        config_path,
        project_dir,
        format,
    }
}

// ── Validation ────────────────────────────────────────────────────────

impl Config {
    /// Validate required fields at startup. Returns Err with human-readable
    /// message listing all missing/invalid fields. Pattern from serial-mcp-server.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors: Vec<String> = Vec::new();

        // Required: dev host IP
        if self.dev_host_ip().is_empty() {
            errors.push("dev_host.ip: required field is empty. Set [[dev_hosts]].ip in .target.toml".into());
        }

        // Required: serial port
        let port = self.serial_target();
        if port == "0" || port == "2000" {
            if self.dev_host_ip().is_empty() {
                // Don't double-error — no host means port is meaningless
            } else {
                errors.push(format!(
                    "dut.serial.port: must be set to a non-zero port (current: {port}). \
                     Set [dut.serial].port in .target.toml"
                ));
            }
        }

        // Optional but warn: login_user
        if self.login_user().is_empty() {
            tracing::warn!("login_user not set — boot-to-shell detection will not work");
        }

        // Validate DUT configs if TOML is present
        if let Some(ref p) = self.config_path {
            if p.exists() {
                match crate::config::parse_dut_configs(p) {
                    Ok(duts) => {
                        if duts.is_empty() {
                            errors.push("No [[dut]] entries found in .target.toml".into());
                        }
                        for dut in &duts {
                            if dut.serial_port.is_empty() || dut.serial_port == "0" {
                                errors.push(format!(
                                    "DUT '{}': serial.port is not set", dut.alias
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        errors.push(format!("Failed to parse [[dut]] from .target.toml: {e}"));
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Serialize tests that manipulate global env vars to prevent
    /// races in parallel test execution (cargo test runs N threads).
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_defaults() {
        let d = defaults();
        assert_eq!(d.get("DEV_HOST_IP").unwrap(), "");
        assert_eq!(d.get("SERIAL_PORT").unwrap(), "2000");
        assert_eq!(d.get("HANG_TIMEOUT").unwrap(), "60");
    }

    #[test]
    fn test_parse_toml_config() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join(".target.toml");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(
            f,
            r#"
[[dev_hosts]]
ip = "10.0.0.99"
user = "linaro"

[serial]
port = 9999

[target]
login_user = "admin"
login_pass = "secret"

[uboot]
interrupt_char = "ctrl_c"
interrupt_strategy = "aggressive"

[relay]
port = 2001
reset_channel = 1
maskrom_channel = 2

[monitor]
hang_timeout = 120
"#
        )
        .unwrap();
        let cfg = parse_toml_config(&conf);
        assert_eq!(cfg.get("DEV_HOST_IP").unwrap(), "10.0.0.99");
        assert_eq!(cfg.get("DEV_HOST_USER").unwrap(), "linaro");
        assert_eq!(cfg.get("SERIAL_PORT").unwrap(), "9999");
        assert_eq!(cfg.get("LOGIN_USER").unwrap(), "admin");
        assert_eq!(cfg.get("LOGIN_PASS").unwrap(), "secret");
        assert_eq!(cfg.get("UBOOT_INTERRUPT_CHAR").unwrap(), "ctrl_c");
        assert_eq!(cfg.get("UBOOT_INTERRUPT_STRATEGY").unwrap(), "aggressive");
        assert_eq!(cfg.get("RELAY_PORT").unwrap(), "2001");
        assert_eq!(cfg.get("RESET_CHANNEL").unwrap(), "1");
        assert_eq!(cfg.get("MASKROM_CHANNEL").unwrap(), "2");
        assert_eq!(cfg.get("HANG_TIMEOUT").unwrap(), "120");
    }

    #[test]
    fn test_toml_config_partial() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join(".target.toml");
        std::fs::write(
            &conf,
            r#"
[[dev_hosts]]
ip = "192.168.1.1"

[serial]
port = 2000
"#,
        )
        .unwrap();
        let cfg = parse_toml_config(&conf);
        assert_eq!(cfg.get("DEV_HOST_IP").unwrap(), "192.168.1.1");
        assert_eq!(cfg.get("SERIAL_PORT").unwrap(), "2000");
        // Not set → absent
        assert!(cfg.get("LOGIN_USER").is_none());
    }

    #[test]
    fn test_toml_config_dev_hosts_single_table() {
        // [[dev_hosts]] array (standard format)
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join(".target.toml");
        std::fs::write(
            &conf,
            r#"
[[dev_hosts]]
alias = "main-host"
ip = "10.0.0.100"
user = "modern"

[serial]
port = 3000
"#,
        )
        .unwrap();
        let cfg = parse_toml_config(&conf);
        assert_eq!(cfg.get("DEV_HOST_IP").unwrap(), "10.0.0.100");
        assert_eq!(cfg.get("DEV_HOST_USER").unwrap(), "modern");
        assert_eq!(cfg.get("DEV_HOST_ALIAS").unwrap(), "main-host");
        assert_eq!(cfg.get("SERIAL_PORT").unwrap(), "3000");
    }

    #[test]
    fn test_toml_config_multi_dev_hosts_multi_dut() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join(".target.toml");
        std::fs::write(
            &conf,
            r#"
[[dev_hosts]]
alias = "rk-board-pc"
ip = "192.168.1.100"
user = "linaro"

[[dev_hosts]]
alias = "lab-pc-2"
ip = "192.168.2.200"
user = "root"

[[dut]]
alias = "rk3576"
dev_host = "rk-board-pc"

[dut.serial]
port = 2000

[dut.target]
login_user = "root"

[dut.uboot]
interrupt_char = "ctrl_c"
interrupt_strategy = "aggressive"

[dut.relay]
port = 2001
reset_ch = 1

[[dut]]
alias = "rk3588"
dev_host = "lab-pc-2"

[dut.serial]
port = 2010

[dut.target]
login_user = "testuser"
login_pass = "pass"

[dut.monitor]
hang_timeout = 90
reference_log = ".dut-serial/rk3588/reference-boot.log"
"#,
        )
        .unwrap();
        let result = parse_dut_configs(&conf).unwrap();
        assert_eq!(result.len(), 2);

        let rk3576 = &result[0];
        assert_eq!(rk3576.alias, "rk3576");
        assert_eq!(rk3576.dev_host_ip, "192.168.1.100");
        assert_eq!(rk3576.dev_host_user, "linaro");
        assert_eq!(rk3576.serial_port, "2000");
        assert_eq!(rk3576.login_user, "root");
        assert_eq!(rk3576.reset_ch, 1);
        assert_eq!(rk3576.relay_port, 2001);

        let rk3588 = &result[1];
        assert_eq!(rk3588.alias, "rk3588");
        assert_eq!(rk3588.dev_host_ip, "192.168.2.200");
        assert_eq!(rk3588.dev_host_user, "root");
        assert_eq!(rk3588.serial_port, "2010");
        assert_eq!(rk3588.login_user, "testuser");
        assert_eq!(rk3588.login_pass, "pass");
    }

    #[test]
    /// Regression test: single-DUT with `[[dev_hosts]]` array format.
    #[test]
    fn test_parse_dut_configs_with_dev_hosts_single_table() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join(".target.toml");
        std::fs::write(
            &conf,
            r#"
[[dev_hosts]]
ip = "192.168.1.105"
user = "linaro"

[serial]
port = 2002

[target]
login_user = "root"
login_pass = ""

[uboot]
interrupt_char = "ctrl_c"
interrupt_strategy = "aggressive"

[relay]
type = "usb-relay"
port = 2000

[monitor]
hang_timeout = 60
max_archived_logs = 10

reference_log = ".dut-serial/reference-boot.log"
"#,
        )
        .unwrap();
        let result = parse_dut_configs(&conf).unwrap();
        assert_eq!(result.len(), 1);
        let dut = &result[0];
        assert_eq!(dut.dev_host_ip, "192.168.1.105");
        assert_eq!(dut.dev_host_user, "linaro");
        assert_eq!(dut.serial_port, "2002");
        assert_eq!(dut.login_user, "root");
        assert_eq!(dut.relay_port, 2000);
    }

    #[test]
    fn test_load_config_toml_preferred() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".dut-serial")).unwrap();
        let conf = tmp.path().join(".target.toml");
        std::fs::write(
            &conf,
            r#"
[[dev_hosts]]
ip = "10.0.0.1"

[serial]
port = 5000
"#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("TARGET_CONF", conf.to_str().unwrap());
        }
        let cfg = load_config();
        unsafe {
            std::env::remove_var("TARGET_CONF");
        }

        assert_eq!(cfg.dev_host_ip(), "10.0.0.1");
        assert_eq!(cfg.format, ConfigFormat::Toml);
    }

    #[test]
    fn test_uboot_interrupt_char_ctrl_c() {
        let mut v = defaults();
        v.insert("UBOOT_INTERRUPT_CHAR".into(), "ctrl_c".into());
        let cfg = Config {
            values: v,
            config_path: None,
            project_dir: None,
            format: ConfigFormat::None,
        };
        assert_eq!(cfg.uboot_interrupt_char(), 0x03);
    }

    #[test]
    fn test_uboot_interrupt_char_allwinner() {
        let mut v = defaults();
        v.insert("UBOOT_INTERRUPT_CHAR".into(), "2".into());
        let cfg = Config {
            values: v,
            config_path: None,
            project_dir: None,
            format: ConfigFormat::None,
        };
        assert_eq!(cfg.uboot_interrupt_char(), b'2');
    }

    // ── DUT IP fallback tests ──────────────────────────────────────────

    /// Helper: build a minimal DutToml with optional serial.ip and relay.ip
    fn make_dut_toml(serial_ip: Option<&str>, relay_ip: Option<&str>) -> DutToml {
        DutToml {
            alias: Some("test-dut".into()),
            dev_host: Some("main".into()),
            serial: Some(SerialToml {
                ip: serial_ip.map(|s| s.to_string()),
                port: Some(toml::Value::Integer(2000)),
                baudrate: None,
            }),
            relay: Some(RelayToml {
                relay_type: Some("usb-relay".into()),
                port: Some(2002),
                ip: relay_ip.map(|s| s.to_string()),
                reset_ch: Some(1),
                maskrom_ch: None,
                recovery_ch: None,
                power_ch: None,
                reset_channel: None,
                maskrom_channel: None,
                recovery_channel: None,
                power_channel: None,
                reset_time_ms: None,
                power_off_time_ms: None,
            }),
            target: None,
            monitor: None,
            flash: None,
            control: None,
            reference_log: None,
            dut_dir: None,
        }
    }

    fn make_dev_hosts() -> Vec<DevHostConfig> {
        vec![DevHostConfig {
            alias: "main".into(),
            ip: "10.0.0.1".into(),
            user: "linaro".into(),
            pass: String::new(),
        }]
    }

    #[test]
    fn test_dut_serial_ip_falls_back_to_dev_host_ip() {
        // dut.serial.ip is NOT set → should use dev_host.ip
        let dut_toml = make_dut_toml(None, Some("10.0.0.1"));
        let dev_hosts = make_dev_hosts();
        let global = HashMap::new();

        let config = dut_toml_to_config(dut_toml, &global, &dev_hosts);

        assert_eq!(config.dev_host_ip, "10.0.0.1");
        assert_eq!(
            config.serial_ip, "10.0.0.1",
            "serial_ip should fall back to dev_host_ip"
        );
    }

    #[test]
    fn test_dut_serial_ip_uses_explicit_value() {
        // dut.serial.ip IS set → should use the explicit value, not dev_host.ip
        let dut_toml = make_dut_toml(Some("192.168.99.1"), None);
        let dev_hosts = make_dev_hosts();
        let global = HashMap::new();

        let config = dut_toml_to_config(dut_toml, &global, &dev_hosts);

        assert_eq!(config.dev_host_ip, "10.0.0.1");
        assert_eq!(
            config.serial_ip, "192.168.99.1",
            "explicit serial_ip should be preserved"
        );
    }

    #[test]
    fn test_dut_relay_ip_falls_back_to_dev_host_ip() {
        // dut.relay.ip is NOT set → should use dev_host.ip
        let dut_toml = make_dut_toml(Some("10.0.0.1"), None);
        let dev_hosts = make_dev_hosts();
        let global = HashMap::new();

        let config = dut_toml_to_config(dut_toml, &global, &dev_hosts);

        assert_eq!(config.dev_host_ip, "10.0.0.1");
        assert_eq!(
            config.relay_ip, "10.0.0.1",
            "relay_ip should fall back to dev_host_ip"
        );
    }

    #[test]
    fn test_dut_relay_ip_uses_explicit_value() {
        // dut.relay.ip IS set → should use the explicit value
        let dut_toml = make_dut_toml(None, Some("192.168.88.1"));
        let dev_hosts = make_dev_hosts();
        let global = HashMap::new();

        let config = dut_toml_to_config(dut_toml, &global, &dev_hosts);

        assert_eq!(config.dev_host_ip, "10.0.0.1");
        assert_eq!(
            config.relay_ip, "192.168.88.1",
            "explicit relay_ip should be preserved"
        );
    }

    #[test]
    fn test_dut_both_ips_fall_back_to_dev_host_ip() {
        // Neither serial.ip nor relay.ip is set → both should fall back
        let dut_toml = make_dut_toml(None, None);
        let dev_hosts = make_dev_hosts();
        let global = HashMap::new();

        let config = dut_toml_to_config(dut_toml, &global, &dev_hosts);

        assert_eq!(config.serial_ip, "10.0.0.1", "serial_ip should fall back");
        assert_eq!(config.relay_ip, "10.0.0.1", "relay_ip should fall back");
    }

    /// End-to-end: parse a full TOML with [[dut]] where serial.ip is empty,
    /// verify the final Config has the correct IP via to_config_map().
    #[test]
    fn test_dut_ip_fallback_in_full_config_map() {
        let dut_toml = make_dut_toml(None, None);
        let dev_hosts = make_dev_hosts();
        let global = defaults();

        let config = dut_toml_to_config(dut_toml, &global, &dev_hosts);
        let cfg_map = config.to_config_map(&global);

        // SERIAL_IP and RELAY_IP should be explicitly set to dev_host.ip
        assert_eq!(cfg_map.get("SERIAL_IP").unwrap(), "10.0.0.1");
        assert_eq!(cfg_map.get("RELAY_IP").unwrap(), "10.0.0.1");
        assert_eq!(cfg_map.get("DEV_HOST_IP").unwrap(), "10.0.0.1");
    }

    #[test]
    fn test_malformed_toml_does_not_panic() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join(".target.toml");
        std::fs::write(&conf, "this is not valid toml {{{").unwrap();
        unsafe { std::env::set_var("TARGET_CONF", conf.to_str().unwrap()) };
        let cfg = load_config(); // must not panic
        unsafe { std::env::remove_var("TARGET_CONF") };
        // Should fall back to defaults
        assert_eq!(cfg.dev_host_ip(), "");
    }

    #[test]
    fn test_missing_all_sections_uses_defaults() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join(".target.toml");
        std::fs::write(&conf, "[[dev_hosts]]\nip = \"10.0.0.1\"\n").unwrap();
        unsafe { std::env::set_var("TARGET_CONF", conf.to_str().unwrap()) };
        let cfg = load_config();
        unsafe { std::env::remove_var("TARGET_CONF") };
        assert_eq!(cfg.dev_host_ip(), "10.0.0.1");
        assert_eq!(cfg.serial_target(), "2000"); // default
        assert_eq!(cfg.login_user(), "root"); // default
    }

    #[test]
    fn test_empty_string_values() {
        let mut v = defaults();
        v.insert("DEV_HOST_IP".into(), "".into());
        let cfg = Config {
            values: v,
            config_path: None,
            project_dir: None,
            format: ConfigFormat::None,
        };
        assert_eq!(cfg.dev_host_ip(), "");
    }

    // ── Regression: DUT alias config propagation ────────────────────────

    /// When TARGET_DUT_ALIAS selects a specific DUT, the resulting config
    /// must contain that DUT's values, not just the global ones.  Regression
    /// for the bug where cmd_serial didn't pass TARGET_DUT_ALIAS — the MCP
    /// would use the first DUT's config regardless of --dut selection.
    #[test]
    fn test_dut_alias_to_config_map_preserves_all_fields() {
        let global = defaults();
        let dut = DutConfig {
            alias: "my-dut".into(),
            dev_host_alias: "main".into(),
            dev_host_ip: "10.0.0.1".into(),
            dev_host_user: "linaro".into(),
            dev_host_pass: String::new(),
            serial_port: "2008".into(),
            serial_ip: String::new(), // fall back to dev_host_ip
            relay_ip: String::new(),
            relay_type: "usb-relay".into(),
            relay_port: 2002,
            reset_ch: 1,
            maskrom_ch: 2,
            recovery_ch: 0,
            power_ch: 0,
            power_off_time_ms: 3000,
            reference_log: ".dut-serial/my-dut/reference-boot.log".into(),
            dut_dir: ".dut-serial/my-dut".into(),
            login_user: "root".into(),
            login_pass: String::new(),
            learner_stage_threshold: 0.0,
            learner_crash_threshold: 0.0,
            crash_patterns: Vec::new(),
        };

        let cfg = dut.to_config_map(&global);

        // DUT-specific values must be present
        assert_eq!(cfg.get("DUT_ALIAS").map(|s| s.as_str()), Some("my-dut"));
        assert_eq!(cfg.get("DEV_HOST_IP").map(|s| s.as_str()), Some("10.0.0.1"));
        assert_eq!(cfg.get("SERIAL_PORT").map(|s| s.as_str()), Some("2008"));
        // SERIAL_IP falls back to dev_host_ip since it was empty
        assert_eq!(cfg.get("RELAY_PORT").map(|s| s.as_str()), Some("2002"));
        assert_eq!(cfg.get("RESET_CHANNEL").map(|s| s.as_str()), Some("1"));
        assert_eq!(cfg.get("MASKROM_CHANNEL").map(|s| s.as_str()), Some("2"));
    }
}
