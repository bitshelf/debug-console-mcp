//! Config loading — TOML (.target.toml) with shell (.target.conf) fallback.
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
        ("SERIAL_PROTOCOL".into(), "raw".into()),
        ("SERIAL_BAUDRATE".into(), "1500000".into()),
        ("LOGIN_USER".into(), "root".into()),
        ("LOGIN_PASS".into(), String::new()),
        ("RELAY_PORT".into(), "0".into()),
        ("RESET_CHANNEL".into(), "0".into()),
        ("MASKROM_CHANNEL".into(), "0".into()),
        ("HANG_TIMEOUT".into(), "60".into()),
        ("HANG_HYSTERESIS".into(), "3".into()),
        ("MAX_ARCHIVED_LOGS".into(), "10".into()),
        ("MAX_LOG_FILE_SIZE".into(), "100".into()),
        ("DUT_DIR".into(), ".dut-serial".into()),
        ("UBOOT_INTERRUPT_STRATEGY".into(), "lava".into()),
        ("UBOOT_INTERRUPT_CHAR".into(), "ctrl_c".into()),
        ("LOCK_DIR".into(), "/tmp/embedded-debug/locks".into()),
        ("REFERENCE_LOG".into(), String::new()),
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
    Shell,
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
        if v.is_empty() { default.to_string() } else { v.to_string() }
    }

    pub fn dev_host_ip(&self) -> String { self.get_str_or("DEV_HOST_IP", "") }
    pub fn serial_target(&self) -> String {
        let v = self.get("SERIAL_PORT");
        if v.is_empty() { return "2000".into(); }
        if v.starts_with("/dev/") || v.starts_with("COM") { return v.to_string(); }
        if v.parse::<u16>().is_ok() { return v.to_string(); }
        "2000".into()
    }
    pub fn relay_port(&self) -> u16 { self.get("RELAY_PORT").parse().unwrap_or(0) }
    pub fn reset_channel(&self) -> u8 { self.get("RESET_CHANNEL").parse().unwrap_or(0) }
    pub fn maskrom_channel(&self) -> u8 { self.get("MASKROM_CHANNEL").parse().unwrap_or(0) }
    pub fn hang_timeout(&self) -> u64 { self.get("HANG_TIMEOUT").parse().unwrap_or(60) }
    pub fn hang_hysteresis(&self) -> u32 { self.get("HANG_HYSTERESIS").parse().unwrap_or(3) }
    pub fn max_archived_logs(&self) -> usize { self.get("MAX_ARCHIVED_LOGS").parse().unwrap_or(10) }
    pub fn max_log_file_size_mb(&self) -> u64 { self.get("MAX_LOG_FILE_SIZE").parse().unwrap_or(100) }
    pub fn dut_dir(&self) -> String { self.get_str_or("DUT_DIR", ".dut-serial") }
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
    pub fn lock_dir(&self) -> String { self.get_str_or("LOCK_DIR", "/tmp/embedded-debug/locks") }
    pub fn login_user(&self) -> String { self.get_str_or("LOGIN_USER", "root") }
    pub fn login_pass(&self) -> String { self.get("LOGIN_PASS").to_string() }
    pub fn reference_log(&self) -> String { self.get("REFERENCE_LOG").to_string() }
}

// ── TOML parsing ──────────────────────────────────────────────────────

#[derive(serde::Deserialize, Default)]
struct TargetToml {
    dev_host: Option<DevHostToml>,
    serial: Option<SerialToml>,
    target: Option<TargetCredsToml>,
    uboot: Option<UbootToml>,
    relay: Option<RelayToml>,
    monitor: Option<MonitorToml>,
    // top-level keys for backward compat
    reference_log: Option<String>,
    lock_dir: Option<String>,
    dut_dir: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct DevHostToml {
    ip: Option<String>,
    user: Option<String>,
    pass: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct SerialToml {
    port: Option<toml::Value>, // can be int or string
    baudrate: Option<toml::Value>,
}

#[derive(serde::Deserialize, Default)]
struct TargetCredsToml {
    login_user: Option<String>,
    login_pass: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct UbootToml {
    interrupt_char: Option<String>,
    interrupt_strategy: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct RelayToml {
    port: Option<u16>,
    reset_channel: Option<u8>,
    maskrom_channel: Option<u8>,
}

#[derive(serde::Deserialize, Default)]
struct MonitorToml {
    hang_timeout: Option<u64>,
    hang_hysteresis: Option<u32>,
    max_archived_logs: Option<usize>,
    max_log_file_size: Option<u64>,
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

    // dev_host
    if let Some(dh) = t.dev_host {
        if let Some(v) = dh.ip { cfg.insert("DEV_HOST_IP".into(), v); }
        if let Some(v) = dh.user { cfg.insert("DEV_HOST_USER".into(), v); }
        if let Some(v) = dh.pass { cfg.insert("DEV_HOST_PASS".into(), v); }
    }
    // serial
    if let Some(s) = t.serial {
        if let Some(v) = s.port { cfg.insert("SERIAL_PORT".into(), toml_value_to_string(&v)); }
        if let Some(v) = s.baudrate { cfg.insert("SERIAL_BAUDRATE".into(), toml_value_to_string(&v)); }
    }
    // target
    if let Some(tg) = t.target {
        if let Some(v) = tg.login_user { cfg.insert("LOGIN_USER".into(), v); }
        if let Some(v) = tg.login_pass { cfg.insert("LOGIN_PASS".into(), v); }
    }
    // uboot
    if let Some(ub) = t.uboot {
        if let Some(v) = ub.interrupt_char { cfg.insert("UBOOT_INTERRUPT_CHAR".into(), v); }
        if let Some(v) = ub.interrupt_strategy { cfg.insert("UBOOT_INTERRUPT_STRATEGY".into(), v); }
    }
    // relay
    if let Some(r) = t.relay {
        if let Some(v) = r.port { cfg.insert("RELAY_PORT".into(), v.to_string()); }
        if let Some(v) = r.reset_channel { cfg.insert("RESET_CHANNEL".into(), v.to_string()); }
        if let Some(v) = r.maskrom_channel { cfg.insert("MASKROM_CHANNEL".into(), v.to_string()); }
    }
    // monitor
    if let Some(m) = t.monitor {
        if let Some(v) = m.hang_timeout { cfg.insert("HANG_TIMEOUT".into(), v.to_string()); }
        if let Some(v) = m.hang_hysteresis { cfg.insert("HANG_HYSTERESIS".into(), v.to_string()); }
        if let Some(v) = m.max_archived_logs { cfg.insert("MAX_ARCHIVED_LOGS".into(), v.to_string()); }
        if let Some(v) = m.max_log_file_size { cfg.insert("MAX_LOG_FILE_SIZE".into(), v.to_string()); }
    }
    // top-level
    if let Some(v) = t.reference_log { cfg.insert("REFERENCE_LOG".into(), v); }
    if let Some(v) = t.lock_dir { cfg.insert("LOCK_DIR".into(), v); }
    if let Some(v) = t.dut_dir { cfg.insert("DUT_DIR".into(), v); }

    cfg
}

// ── Shell config parsing ──────────────────────────────────────────────

fn parse_shell_config(path: &Path) -> HashMap<String, String> {
    let mut cfg = HashMap::new();
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return cfg,
    };

    let re = regex::Regex::new(r#"^(\w+)=(?:"([^"]*?)"|'([^']*?)'|([^\s#]*))\s*(?:#.*)?$"#).unwrap();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some(caps) = re.captures(line) {
            let raw_key = caps.get(1).unwrap().as_str().to_string();
            let key = raw_key
                .strip_prefix("RK_")
                .or_else(|| raw_key.strip_prefix("LR_"))
                .unwrap_or(&raw_key)
                .to_string();
            let value = caps
                .get(2)
                .or_else(|| caps.get(3))
                .or_else(|| caps.get(4))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            cfg.insert(key, value);
        }
    }
    cfg
}

// ── Config path discovery ─────────────────────────────────────────────

const FORBIDDEN_ROOTS: &[&str] = &["/tmp", "/var/tmp", "/dev/shm", "/run", "/proc", "/sys", "/dev"];
const PROJECT_MARKERS: &[&str] = &[".dut-serial", ".claude", "build/envsetup.sh", "device/rockchip"];

fn is_valid_project_dir(d: &Path) -> bool {
    let resolved = d.canonicalize().unwrap_or_else(|_| d.to_path_buf());
    for forbidden in FORBIDDEN_ROOTS {
        if resolved == Path::new(forbidden) {
            return false;
        }
    }
    for marker in PROJECT_MARKERS {
        if d.join(marker).exists() {
            return true;
        }
    }
    false
}

/// Find config file: tries .target.toml first, then .target.conf (backward compat).
fn find_config_path() -> Option<PathBuf> {
    // TARGET_CONF env var overrides
    if let Ok(env) = std::env::var("TARGET_CONF") {
        let p = PathBuf::from(&env);
        if p.exists() {
            let parent = p.parent().unwrap_or(Path::new("."));
            if is_valid_project_dir(parent) {
                return Some(p);
            }
        }
    }
    let mut d = std::env::current_dir().ok()?;
    loop {
        // Prefer TOML
        for name in &[".target.toml", ".target.conf"] {
            let candidate = d.join(name);
            if candidate.exists() && is_valid_project_dir(&d) {
                return Some(candidate);
            }
        }
        if !d.pop() {
            break;
        }
    }
    None
}

/// Load config: defaults + file overrides (TOML preferred, shell fallback)
pub fn load_config() -> Config {
    let mut values = defaults();
    let path = find_config_path();

    let format = match path {
        Some(ref p) if p.extension().is_some_and(|e| e == "toml") => ConfigFormat::Toml,
        Some(ref p) if p.extension().is_some_and(|e| e == "conf") => ConfigFormat::Shell,
        Some(_) => ConfigFormat::Shell, // unknown extension → try shell
        None => ConfigFormat::None,
    };

    if let Some(ref p) = path {
        let file_cfg = match format {
            ConfigFormat::Toml => parse_toml_config(p),
            ConfigFormat::Shell | ConfigFormat::None => parse_shell_config(p),
        };
        values.extend(file_cfg);
    }

    let config_path = path;
    let project_dir = config_path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()));

    Config { values, config_path, project_dir, format }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_defaults() {
        let d = defaults();
        assert_eq!(d.get("DEV_HOST_IP").unwrap(), "");
        assert_eq!(d.get("SERIAL_PORT").unwrap(), "2000");
        assert_eq!(d.get("HANG_TIMEOUT").unwrap(), "60");
    }

    #[test]
    fn test_parse_shell_config_simple() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join("test.conf");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, "DEV_HOST_IP=10.0.0.1").unwrap();
        writeln!(f, "SERIAL_PORT=3000").unwrap();
        writeln!(f, "# comment").unwrap();
        writeln!(f, "LOGIN_USER=admin").unwrap();
        let cfg = parse_shell_config(&conf);
        assert_eq!(cfg.get("DEV_HOST_IP").unwrap(), "10.0.0.1");
        assert_eq!(cfg.get("SERIAL_PORT").unwrap(), "3000");
        assert_eq!(cfg.get("LOGIN_USER").unwrap(), "admin");
    }

    #[test]
    fn test_parse_shell_config_quotes() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join("test.conf");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, r#"LOGIN_PASS="my password""#).unwrap();
        writeln!(f, "LOGIN_USER='root'").unwrap();
        let cfg = parse_shell_config(&conf);
        assert_eq!(cfg.get("LOGIN_PASS").unwrap(), "my password");
        assert_eq!(cfg.get("LOGIN_USER").unwrap(), "root");
    }

    #[test]
    fn test_parse_toml_config() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join(".target.toml");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, r#"
[dev_host]
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
"#).unwrap();
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
        std::fs::write(&conf, r#"
[dev_host]
ip = "192.168.1.1"

[serial]
port = 2000
"#).unwrap();
        let cfg = parse_toml_config(&conf);
        assert_eq!(cfg.get("DEV_HOST_IP").unwrap(), "192.168.1.1");
        assert_eq!(cfg.get("SERIAL_PORT").unwrap(), "2000");
        // Not set → absent
        assert!(cfg.get("LOGIN_USER").is_none());
    }

    #[test]
    fn test_load_config_shell_fallback() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".dut-serial")).unwrap();
        let conf = tmp.path().join(".target.conf");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, "DEV_HOST_IP=172.16.0.1").unwrap();
        writeln!(f, "SERIAL_PORT=9000").unwrap();
        drop(f);

        unsafe { std::env::set_var("TARGET_CONF", conf.to_str().unwrap()); }
        let cfg = load_config();
        unsafe { std::env::remove_var("TARGET_CONF"); }

        assert_eq!(cfg.dev_host_ip(), "172.16.0.1");
        assert_eq!(cfg.serial_target().parse::<u16>().unwrap(), 9000);
        assert_eq!(cfg.format, ConfigFormat::Shell);
        assert_eq!(cfg.hang_timeout(), 60); // default
    }

    #[test]
    fn test_load_config_toml_preferred() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".dut-serial")).unwrap();
        let conf = tmp.path().join(".target.toml");
        std::fs::write(&conf, r#"
[dev_host]
ip = "10.0.0.1"

[serial]
port = 5000
"#).unwrap();

        unsafe { std::env::set_var("TARGET_CONF", conf.to_str().unwrap()); }
        let cfg = load_config();
        unsafe { std::env::remove_var("TARGET_CONF"); }

        assert_eq!(cfg.dev_host_ip(), "10.0.0.1");
        assert_eq!(cfg.format, ConfigFormat::Toml);
    }

    #[test]
    fn test_uboot_interrupt_char_ctrl_c() {
        let mut v = defaults();
        v.insert("UBOOT_INTERRUPT_CHAR".into(), "ctrl_c".into());
        let cfg = Config { values: v, config_path: None, project_dir: None, format: ConfigFormat::None };
        assert_eq!(cfg.uboot_interrupt_char(), 0x03);
    }

    #[test]
    fn test_uboot_interrupt_char_allwinner() {
        let mut v = defaults();
        v.insert("UBOOT_INTERRUPT_CHAR".into(), "2".into());
        let cfg = Config { values: v, config_path: None, project_dir: None, format: ConfigFormat::None };
        assert_eq!(cfg.uboot_interrupt_char(), b'2');
    }
}
