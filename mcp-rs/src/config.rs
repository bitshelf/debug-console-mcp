//! Config loading — 解析 shell 风格 .target.conf，递归向上查找。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 默认配置值
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
        ("LOCK_DIR".into(), "/tmp/embedded-debug/locks".into()),
        ("REFERENCE_LOG".into(), String::new()),
    ])
}

/// 已加载的配置
#[derive(Debug, Clone)]
pub struct Config {
    pub values: HashMap<String, String>,
    pub config_path: Option<PathBuf>,
    pub project_dir: Option<PathBuf>,
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
    pub fn dev_host_user(&self) -> String { self.get("DEV_HOST_USER").to_string() }
    pub fn dev_host_pass(&self) -> String { self.get("DEV_HOST_PASS").to_string() }
    /// Returns serial target: "2000" for TCP port, "/dev/ttyUSB0" for local device.
    /// 默认 2000 (ser2net TCP); 如果 SERIAL_PORT 是 /dev/ 路径则直接返回路径。
    pub fn serial_target(&self) -> String {
        let v = self.get("SERIAL_PORT");
        if v.is_empty() { return "2000".into(); }
        if v.starts_with("/dev/") || v.starts_with("COM") { return v.to_string(); }
        // 验证是否为合法端口号
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
    pub fn lock_dir(&self) -> String { self.get_str_or("LOCK_DIR", "/tmp/embedded-debug/locks") }
    pub fn interrupt_strategy(&self) -> String { self.get_str_or("UBOOT_INTERRUPT_STRATEGY", "lava") }
    pub fn login_user(&self) -> String { self.get_str_or("LOGIN_USER", "root") }
    pub fn login_pass(&self) -> String { self.get("LOGIN_PASS").to_string() }
    pub fn reference_log(&self) -> String { self.get("REFERENCE_LOG").to_string() }
}

/// 解析 shell 风格 key=value 文件 (支持 export, 引号, 注释)
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
            let key = caps.get(1).unwrap().as_str().to_string();
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

/// 递归向上查找 .target.conf
fn find_config_path() -> Option<PathBuf> {
    if let Ok(env) = std::env::var("TARGET_CONF") {
        let p = PathBuf::from(&env);
        if p.exists() {
            return Some(p);
        }
    }
    let mut d = std::env::current_dir().ok()?;
    loop {
        let candidate = d.join(".target.conf");
        if candidate.exists() {
            return Some(candidate);
        }
        if !d.pop() {
            break;
        }
    }
    None
}

/// 加载配置: 默认值 + 文件覆盖
pub fn load_config() -> Config {
    let mut values = defaults();
    let path = find_config_path();

    let config_path = path.clone();
    // project_dir = .target.conf 所在目录, .dut-serial/ 与此同目录
    let project_dir = path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()));

    if let Some(ref p) = path {
        let file_cfg = parse_shell_config(p);
        values.extend(file_cfg);
    }

    Config { values, config_path, project_dir }
}

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
        assert_eq!(d.get("HANG_HYSTERESIS").unwrap(), "3");
        assert_eq!(d.get("MAX_ARCHIVED_LOGS").unwrap(), "10");
        assert_eq!(d.get("DUT_DIR").unwrap(), ".dut-serial");
    }

    #[test]
    fn test_parse_shell_config_simple() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join("test.conf");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, "DEV_HOST_IP=10.0.0.1").unwrap();
        writeln!(f, "SERIAL_PORT=3000").unwrap();
        writeln!(f, "# comment").unwrap();
        writeln!(f, "").unwrap();
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
        writeln!(f, "DEV_HOST_IP=192.168.1.1").unwrap();

        let cfg = parse_shell_config(&conf);
        assert_eq!(cfg.get("LOGIN_PASS").unwrap(), "my password");
        assert_eq!(cfg.get("LOGIN_USER").unwrap(), "root");
        assert_eq!(cfg.get("DEV_HOST_IP").unwrap(), "192.168.1.1");
    }

    #[test]
    fn test_parse_shell_config_export() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join("test.conf");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, "export DEV_HOST_IP=10.0.0.2").unwrap();
        writeln!(f, "export SERIAL_PORT=4000").unwrap();

        let cfg = parse_shell_config(&conf);
        assert_eq!(cfg.get("DEV_HOST_IP").unwrap(), "10.0.0.2");
        assert_eq!(cfg.get("SERIAL_PORT").unwrap(), "4000");
    }

    #[test]
    fn test_parse_shell_config_inline_comment() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join("test.conf");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, "DEV_HOST_IP=10.0.0.3 # this is a comment").unwrap();

        let cfg = parse_shell_config(&conf);
        assert_eq!(cfg.get("DEV_HOST_IP").unwrap(), "10.0.0.3");
    }

    #[test]
    fn test_parse_shell_config_empty() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join("test.conf");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, "# only comments").unwrap();
        writeln!(f, "").unwrap();

        let cfg = parse_shell_config(&conf);
        assert!(cfg.is_empty());
    }

    #[test]
    fn test_config_accessors() {
        let mut values = defaults();
        values.insert("DEV_HOST_IP".into(), "10.0.0.5".into());
        values.insert("SERIAL_PORT".into(), "5000".into());
        values.insert("RELAY_PORT".into(), "5001".into());
        values.insert("RESET_CHANNEL".into(), "2".into());
        values.insert("HANG_TIMEOUT".into(), "120".into());

        let cfg = Config {
            values,
            config_path: None,
            project_dir: None,
        };

        assert_eq!(cfg.dev_host_ip(), "10.0.0.5");
        assert_eq!(cfg.serial_target().parse::<u16>().unwrap(), 5000);
        assert_eq!(cfg.relay_port(), 5001);
        assert_eq!(cfg.reset_channel(), 2);
        assert_eq!(cfg.hang_timeout(), 120);
        assert_eq!(cfg.hang_hysteresis(), 3); // default
        assert_eq!(cfg.max_archived_logs(), 10); // default
        assert_eq!(cfg.dut_dir(), ".dut-serial"); // default
    }

    #[test]
    fn test_config_get_missing_key() {
        let cfg = Config {
            values: HashMap::new(),
            config_path: None,
            project_dir: None,
        };
        assert_eq!(cfg.get("MISSING"), "");
        assert_eq!(cfg.get_int("MISSING"), 0);
        assert_eq!(cfg.get_str_or("MISSING", "default"), "default");
    }

    #[test]
    fn test_config_override_defaults() {
        let tmp = TempDir::new().unwrap();
        let conf = tmp.path().join(".target.conf");
        let mut f = std::fs::File::create(&conf).unwrap();
        writeln!(f, "DEV_HOST_IP=172.16.0.1").unwrap();
        writeln!(f, "SERIAL_PORT=9000").unwrap();
        drop(f);

        // Set env var to point to our config (unsafe in Rust 2024)
        // SAFETY: single-threaded test
        unsafe {
            std::env::set_var("TARGET_CONF", conf.to_str().unwrap());
        }
        let cfg = load_config();
        // SAFETY: cleanup
        unsafe {
            std::env::remove_var("TARGET_CONF");
        }

        assert_eq!(cfg.dev_host_ip(), "172.16.0.1");
        assert_eq!(cfg.serial_target().parse::<u16>().unwrap(), 9000);
        assert_eq!(cfg.hang_timeout(), 60); // default preserved
    }
}
