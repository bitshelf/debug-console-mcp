//! Integration tests for dutabo CLI.
//! Run with: cargo test --test dutabo_tests

use std::process::Command;

fn dutabo_bin() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_dutabo") {
        let p = std::path::PathBuf::from(&path);
        if p.exists() {
            return p;
        }
    }
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        for subdir in &["target/debug/dutabo", "target/release/dutabo"] {
            let p = std::path::PathBuf::from(&manifest).join(subdir);
            if p.exists() {
                return p;
            }
        }
    }
    std::path::PathBuf::from("target/debug/dutabo")
}

fn make_config(dir: &std::path::Path, content: &str) {
    std::fs::write(dir.join(".target.toml"), content).unwrap();
}

// ── list ────────────────────────────────────────────────────────────────

#[test]
fn test_list_single_dut() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        r#"
[dev_host]
ip = "10.0.0.1"
user = "test"
[serial]
port = 2000
[target]
login_user = "root"
"#,
    );
    let out = Command::new(dutabo_bin())
        .arg("list")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("DUT:"), "{stdout}");
    assert!(stdout.contains("2000"), "{stdout}");
}

#[test]
fn test_list_multi_dut() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        r#"
[[dev_hosts]]
alias = "pc1"
ip = "10.0.0.1"
user = "test"
[[dut]]
alias = "board-a"
dev_host = "pc1"
[dut.serial]
port = 2000
[dut.target]
login_user = "root"
[[dut]]
alias = "board-b"
dev_host = "pc1"
[dut.serial]
port = 2010
[dut.target]
login_user = "admin"
"#,
    );
    let out = Command::new(dutabo_bin())
        .arg("list")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("board-a"), "{stdout}");
    assert!(stdout.contains("board-b"), "{stdout}");
    assert!(stdout.contains("pc1"), "{stdout}");
}

#[test]
fn test_list_backward_compat() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        r#"
[dev_host]
ip = "192.168.1.100"
user = "linaro"
[serial]
port = 9999
"#,
    );
    let out = Command::new(dutabo_bin())
        .arg("list")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("9999"));
}

#[test]
fn test_list_no_config() {
    let tmp = tempfile::TempDir::new().unwrap();
    let out = Command::new(dutabo_bin())
        .arg("list")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains(".target.toml"));
}

#[test]
fn test_list_empty_dut_default() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        "[dev_host]\nip = \"10.0.0.1\"\n[serial]\nport = 2000\n",
    );
    let out = Command::new(dutabo_bin())
        .arg("list")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("default"));
}

// ── help / unknown ──────────────────────────────────────────────────────

#[test]
fn test_help_output() {
    let out = Command::new(dutabo_bin()).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    for cmd in &[
        "list",
        "state",
        "serial",
        "reboot",
        "uboot",
        "maskrom",
        "uf",
        "flash-kernel",
    ] {
        assert!(stderr.contains(cmd), "help missing '{cmd}'");
    }
}

#[test]
fn test_unknown_command() {
    let out = Command::new(dutabo_bin())
        .arg("nonexistent_cmd")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("Unknown command"));
}

// ── state / reboot / uboot / maskrom (no server) ────────────────────────

#[test]
fn test_state_without_server() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        "[dev_host]\nip = \"127.0.0.1\"\n[serial]\nport = 59999\n",
    );
    let out = Command::new(dutabo_bin())
        .arg("state")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stderr.contains("not reachable") || stderr.contains("MCP") || stdout.contains("State:"),
        "stderr={stderr}\nstdout={stdout}"
    );
}

#[test]
fn test_reboot_without_server() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        "[dev_host]\nip = \"127.0.0.1\"\n[serial]\nport = 59999\n",
    );
    let out = Command::new(dutabo_bin())
        .arg("reboot")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stderr.contains("not reachable") || stderr.contains("MCP") || !stdout.trim().is_empty(),
        "stderr={stderr}\nstdout={stdout}"
    );
}

#[test]
fn test_uboot_without_server() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        "[dev_host]\nip = \"127.0.0.1\"\n[serial]\nport = 59999\n",
    );
    let out = Command::new(dutabo_bin())
        .arg("uboot")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stderr.contains("not reachable") || stderr.contains("MCP") || !stdout.trim().is_empty(),
        "stderr={stderr}\nstdout={stdout}"
    );
}

#[test]
fn test_maskrom_without_server() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        "[dev_host]\nip = \"127.0.0.1\"\n[serial]\nport = 59999\n",
    );
    let out = Command::new(dutabo_bin())
        .arg("maskrom")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stderr.contains("not reachable") || stderr.contains("MCP") || !stdout.trim().is_empty(),
        "stderr={stderr}\nstdout={stdout}"
    );
}

// ── flash ───────────────────────────────────────────────────────────────

#[test]
fn test_uf_missing_arg() {
    let out = Command::new(dutabo_bin()).arg("uf").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("Usage"));
}

#[test]
fn test_flash_kernel_missing_arg() {
    let out = Command::new(dutabo_bin())
        .arg("flash-kernel")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("Usage"));
}

#[test]
fn test_uf_nonexistent_image() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        "[dev_host]\nip = \"127.0.0.1\"\n[serial]\nport = 59999\n[flash]\ntool = \"upgrade_tool\"\nfull_image_cmd = \"uf {image}\"\n",
    );
    let out = Command::new(dutabo_bin())
        .args(["uf", "/tmp/nonexistent_12345.img"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("Error"),
        "{stderr}"
    );
}

// ── --dut flag ──────────────────────────────────────────────────────────

#[test]
fn test_multi_dut_requires_selection() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        r#"
[[dev_hosts]]
alias = "pc1"
ip = "10.0.0.1"
user = "test"
[[dut]]
alias = "dut-a"
dev_host = "pc1"
[dut.serial]
port = 2000
[[dut]]
alias = "dut-b"
dev_host = "pc1"
[dut.serial]
port = 2010
"#,
    );
    let out = Command::new(dutabo_bin())
        .arg("state")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Multiple") || stderr.contains("Select"),
        "{stderr}"
    );
}

#[test]
fn test_dut_flag_selects() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        r#"
[[dev_hosts]]
alias = "pc1"
ip = "10.0.0.1"
user = "test"
[[dut]]
alias = "dut-a"
dev_host = "pc1"
[dut.serial]
port = 2000
[[dut]]
alias = "dut-b"
dev_host = "pc1"
[dut.serial]
port = 2010
"#,
    );
    let out = Command::new(dutabo_bin())
        .args(["--dut", "dut-a", "state"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("Multiple DUTs"), "{stderr}");
}

#[test]
fn test_dut_flag_invalid() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        r#"
[[dev_hosts]]
alias = "pc1"
ip = "10.0.0.1"
user = "test"
[[dut]]
alias = "real-dut"
dev_host = "pc1"
[dut.serial]
port = 2000
"#,
    );
    let out = Command::new(dutabo_bin())
        .args(["--dut", "nonexistent", "state"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("Available"),
        "{stderr}"
    );
}

// ── --mcp-port ──────────────────────────────────────────────────────────

#[test]
fn test_mcp_port_flag() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        "[dev_host]\nip = \"127.0.0.1\"\n[serial]\nport = 59999\n",
    );
    let out = Command::new(dutabo_bin())
        .args(["--mcp-port", "12345", "state"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("12345") || stderr.contains("MCP") || stderr.contains("not reachable"),
        "{stderr}"
    );
}

// ── relay disabled by default ───────────────────────────────────────────

#[test]
fn test_relay_disabled() {
    let tmp = tempfile::TempDir::new().unwrap();
    make_config(
        tmp.path(),
        "[dev_host]\nip = \"10.0.0.1\"\n[serial]\nport = 2000\n[relay]\n# port = 2001\n",
    );
    let out = Command::new(dutabo_bin())
        .arg("list")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("reset_ch=0"));
}
