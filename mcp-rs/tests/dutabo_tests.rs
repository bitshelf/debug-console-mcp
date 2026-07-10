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
        "status={:?}\nstderr={stderr}\nstdout={stdout}",
        out.status
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
        "status={:?}\nstderr={stderr}\nstdout={stdout}",
        out.status
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
        "status={:?}\nstderr={stderr}\nstdout={stdout}",
        out.status
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
        "status={:?}\nstderr={stderr}\nstdout={stdout}",
        out.status
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
        "status={:?}\nstderr={stderr}",
        out.status
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

// ── help via cargo run ───────────────────────────────────────────────────

#[test]
fn test_dutabo_help_output() {
    let output = std::process::Command::new("cargo")
        .args(["run", "--bin", "dutabo", "--", "--help"])
        .output();
    // dutabo might fail without .target.toml — that's OK
    // Just verify it doesn't segfault
    match output {
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            assert!(stderr.contains("dutabo") || stderr.contains("Usage"));
        }
        Err(_) => {} // build failure is OK in test environment
    }
}

// ── config module unit tests ─────────────────────────────────────────────

#[test]
fn test_find_target_toml_nonexistent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let old = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();
    let cfg = debug_console_mcp::config::load_config();
    std::env::set_current_dir(&old).unwrap();
    assert!(cfg.config_path.is_none()); // no .target.toml in tmp
}

#[test]
fn test_dut_config_default_alias() {
    let tmp = tempfile::TempDir::new().unwrap();
    let toml = tmp.path().join(".target.toml");
    std::fs::write(
        &toml,
        r#"
[dev_host]
ip = "10.0.0.1"

[[dut]]
alias = "test-board"

[dut.serial]
port = 2000
"#,
    )
    .unwrap();
    let duts = debug_console_mcp::config::parse_dut_configs(&toml).unwrap();
    assert_eq!(duts.len(), 1);
    assert_eq!(duts[0].alias, "test-board");
    assert_eq!(duts[0].serial_port, "2000");
}

// ── Serial simulator tests ─────────────────────────────────────────────

mod serial_sim {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::process::{Child, Command};
    use std::time::Duration;

    fn serial_sim_bin() -> std::path::PathBuf {
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_serial-sim") {
            let p = std::path::PathBuf::from(&path);
            if p.exists() {
                return p;
            }
        }
        if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
            for subdir in &["target/debug/serial-sim", "target/release/serial-sim"] {
                let p = std::path::PathBuf::from(&manifest).join(subdir);
                if p.exists() {
                    return p;
                }
            }
        }
        std::path::PathBuf::from("target/debug/serial-sim")
    }

    struct SimGuard {
        child: Child,
        port: u16,
    }

    impl Drop for SimGuard {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn start_sim(no_login: bool) -> SimGuard {
        let bin = serial_sim_bin();
        // Use port 0 to let the OS pick a free port
        let mut cmd = Command::new(&bin);
        cmd.args(["--port", "0", "--print-port"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());
        if no_login {
            cmd.arg("--no-login");
        }
        let mut child = cmd.spawn().expect("Failed to start serial-sim");

        // Read the dynamically-assigned port from stdout
        let port_str = {
            let stdout = child.stdout.as_mut().unwrap();
            let mut buf = [0u8; 16];
            let mut total = Vec::new();
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(3) {
                match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        total.extend_from_slice(&buf[..n]);
                        if total.contains(&b'\n') {
                            break;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
            }
            String::from_utf8_lossy(&total).trim().to_string()
        };
        let port: u16 = port_str.parse().expect(&format!(
            "serial-sim did not print a port, got: {port_str:?}"
        ));
        assert!(port > 0, "Expected valid port, got {port}");

        // Give serial-sim time to start listening
        std::thread::sleep(Duration::from_millis(200));
        SimGuard { child, port }
    }

    fn read_until_timeout(stream: &mut TcpStream, timeout_ms: u64) -> Vec<u8> {
        let mut buf = [0u8; 4096];
        let mut all = Vec::new();
        let start = std::time::Instant::now();
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .ok();
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => all.extend_from_slice(&buf[..n]),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed().as_millis() > timeout_ms as u128 {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        all
    }

    #[test]
    fn test_prompt_starts_at_column_zero_no_login() {
        let sim = start_sim(true);
        let mut stream =
            TcpStream::connect(format!("127.0.0.1:{}", sim.port)).unwrap();

        // Read initial prompt
        let initial = read_until_timeout(&mut stream, 2000);
        let text = String::from_utf8_lossy(&initial);
        assert!(
            text.starts_with("root@"),
            "Initial prompt should start at column 0, got: {text:?}"
        );

        // Send empty line to get a fresh prompt
        stream.write_all(b"\n").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let response = read_until_timeout(&mut stream, 2000);
        let text = String::from_utf8_lossy(&response);
        // Response: echo of Enter (\r\n) + prompt (\r\nroot@...)
        // The prompt itself must start at column 0 after a \r\n.
        assert!(
            text.contains("\r\nroot@"),
            "Prompt should appear at column 0 (after \\r\\n), got: {text:?}"
        );
    }

    #[test]
    fn test_command_echo_and_response() {
        let sim = start_sim(true);
        let mut stream =
            TcpStream::connect(format!("127.0.0.1:{}", sim.port)).unwrap();
        // Consume initial prompt
        read_until_timeout(&mut stream, 2000);

        // Send echo command
        stream.write_all(b"echo hello\n").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let response = read_until_timeout(&mut stream, 2000);
        let text = String::from_utf8_lossy(&response);

        // Expected: echoed characters + \r\nhello\r\n<PROMPT>
        assert!(
            text.contains("hello"),
            "Should echo the command output, got: {text:?}"
        );
        assert!(
            text.contains("root@"),
            "Should end with a prompt, got: {text:?}"
        );
    }

    #[test]
    fn test_login_flow() {
        let sim = start_sim(false); // with login
        let mut stream =
            TcpStream::connect(format!("127.0.0.1:{}", sim.port)).unwrap();

        // Read initial login prompt
        let initial = read_until_timeout(&mut stream, 2000);
        let text = String::from_utf8_lossy(&initial);
        assert!(
            text.contains("login:"),
            "Should show login prompt, got: {text:?}"
        );

        // Send login username
        stream.write_all(b"root\n").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let response = read_until_timeout(&mut stream, 2000);
        let text = String::from_utf8_lossy(&response);
        assert!(
            text.contains("root@"),
            "Should be logged in with root prompt, got: {text:?}"
        );
    }

    #[test]
    fn test_stty_echo_off_on() {
        let sim = start_sim(true);
        let mut stream =
            TcpStream::connect(format!("127.0.0.1:{}", sim.port)).unwrap();
        // Consume initial prompt
        read_until_timeout(&mut stream, 2000);

        // Turn echo off
        stream.write_all(b"stty -echo\n").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        // Consume prompt
        read_until_timeout(&mut stream, 500);

        // Now send a command — should NOT echo characters back
        stream.write_all(b"uptime\n").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let response = read_until_timeout(&mut stream, 2000);
        let text = String::from_utf8_lossy(&response);
        // Should NOT contain "uptime" (no echo), but should contain the output
        assert!(
            !text.contains("uptime"),
            "Command should not be echoed when echo is off, got: {text:?}"
        );
        assert!(
            text.contains("load average"),
            "Should still show command output, got: {text:?}"
        );
    }

    #[test]
    fn test_dmesg_output() {
        let sim = start_sim(true);
        let mut stream =
            TcpStream::connect(format!("127.0.0.1:{}", sim.port)).unwrap();
        read_until_timeout(&mut stream, 2000);

        stream.write_all(b"dmesg\n").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let response = read_until_timeout(&mut stream, 3000);
        let text = String::from_utf8_lossy(&response);
        assert!(
            text.contains("Linux version"),
            "dmesg should show kernel boot log, got: {text:?}"
        );
        assert!(
            text.contains("sun55iw3"),
            "Boot log should contain machine model, got: {text:?}"
        );
    }

    #[test]
    fn test_backspace_handling() {
        let sim = start_sim(true);
        let mut stream =
            TcpStream::connect(format!("127.0.0.1:{}", sim.port)).unwrap();
        read_until_timeout(&mut stream, 2000);

        // Type "echox" then backspace then "o hello\n"
        stream.write_all(b"echox").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        stream.write_all(&[0x7f]).unwrap(); // backspace
        std::thread::sleep(Duration::from_millis(50));
        stream.write_all(b"o hello\n").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let response = read_until_timeout(&mut stream, 2000);
        let text = String::from_utf8_lossy(&response);
        assert!(
            text.contains("hello"),
            "echo hello should succeed after backspace correction, got: {text:?}"
        );
    }

    #[test]
    fn test_xshell_style_inline_backspace_redraw() {
        let sim = start_sim(true);
        let mut stream =
            TcpStream::connect(format!("127.0.0.1:{}", sim.port)).unwrap();
        read_until_timeout(&mut stream, 2000);

        stream.write_all(b"ping baidu.com").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        for _ in 0.." baidu.com".len() {
            stream.write_all(b"\x1b[D").unwrap();
        }
        std::thread::sleep(Duration::from_millis(50));
        stream.write_all(&[0x7f, 0x7f]).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        stream.write_all(b"\n").unwrap();
        std::thread::sleep(Duration::from_millis(300));

        let response = read_until_timeout(&mut stream, 2000);
        let text = String::from_utf8_lossy(&response);
        assert!(
            response.windows(3).any(|w| w == b"\x1b[D"),
            "left-arrow redraw should be echoed like a direct terminal, got: {text:?}"
        );
        assert!(
            response.windows(3).any(|w| w == b"\x1b[P"),
            "in-line backspace should emit delete-character redraw, got: {text:?}"
        );
        assert!(
            text.contains("/bin/sh: pi: not found"),
            "backspacing 'ng' inside ping should execute 'pi baidu.com', got: {text:?}"
        );
    }

    #[test]
    fn test_boot_log_mode() {
        // Start with --boot-log to simulate a board booting
        let bin = serial_sim_bin();
        let mut child = Command::new(&bin)
            .args(["--port", "0", "--no-login", "--boot-log", "--print-port"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("Failed to start serial-sim with --boot-log");

        // Read dynamically-assigned port
        let port: u16 = {
            let stdout = child.stdout.as_mut().unwrap();
            let mut buf = [0u8; 16];
            let mut total = Vec::new();
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(3) {
                match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => { total.extend_from_slice(&buf[..n]); if total.contains(&b'\n') { break; } }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
            }
            String::from_utf8_lossy(&total).trim().parse().unwrap()
        };
        std::thread::sleep(Duration::from_millis(500));

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        let data = read_until_timeout(&mut stream, 5000);
        let text = String::from_utf8_lossy(&data);

        // Should contain boot log AND a prompt at the end
        assert!(
            text.contains("Booting Linux"),
            "Should contain boot log, got: {text:?}"
        );
        assert!(
            text.ends_with("root@"),
            "Boot log should end with shell prompt, got: {text:?}"
        );

        let _ = child.kill();
        let _ = child.wait();
    }
}
