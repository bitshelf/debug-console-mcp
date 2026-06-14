//! dutabo — DUT control CLI for embedded Linux targets.
//!
//! Architecture:
//!   dutabo serial → pause Agent → nc ser2net → resume Agent
//!   dutabo state/reboot/etc → MCP HTTP → Agent

use std::path::PathBuf;

const MCP_DEFAULT_PORT: u16 = 3000;
const MC_PORT_BASE: u16 = 3001;
const MC_PORT_RANGE: u16 = 99;

static INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_usage();
        return;
    }

    let mut dut_alias: Option<String> = None;
    let mut mcp_port: u16 = std::env::var("MCP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(MCP_DEFAULT_PORT);
    let mut mcp_port_explicit = false;
    let mut cmd: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--dut" && i + 1 < args.len() {
            dut_alias = Some(args[i + 1].clone());
            i += 2;
        } else if args[i] == "--mcp-port" && i + 1 < args.len() {
            mcp_port = args[i + 1].parse().unwrap_or(MCP_DEFAULT_PORT);
            mcp_port_explicit = true;
            i += 2;
        } else if !args[i].starts_with('-') && cmd.is_none() {
            cmd = Some(args[i].clone());
            i += 1;
        } else {
            positional.push(args[i].clone());
            i += 1;
        }
    }

    let cmd = match cmd {
        Some(c) => c,
        None => {
            print_usage();
            return;
        }
    };

    if !matches!(
        cmd.as_str(),
        "list" | "state" | "serial" | "reboot" | "uboot" | "maskrom" | "uf" | "flash-kernel"
    ) {
        eprintln!("Unknown command: {cmd}");
        print_usage();
        return;
    }
    if cmd == "uf" && positional.is_empty() {
        eprintln!("Usage: dutabo uf <image>");
        return;
    }
    if cmd == "flash-kernel" && positional.is_empty() {
        eprintln!("Usage: dutabo flash-kernel <image>");
        return;
    }

    // Auto-detect MCP port: .mcp.json url → hash-based → default
    if !mcp_port_explicit {
        if let Some(port) = detect_mcp_port_from_config() {
            mcp_port = port;
        } else if let Some(ref toml) = find_target_toml() {
            mcp_port = project_mcp_port(toml.parent().unwrap_or(std::path::Path::new(".")));
        }
    }

    let toml_path = find_target_toml();
    let duts = match &toml_path {
        Some(p) => match debug_console_mcp::config::parse_dut_configs(p) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{e}");
                return;
            }
        },
        None => {
            eprintln!("No .target.toml found.");
            return;
        }
    };

    if cmd == "list" {
        cmd_list(&duts);
        return;
    }

    let dut = match select_dut(&duts, dut_alias) {
        Some(d) => d,
        None => return,
    };

    match cmd.as_str() {
        "state" => cmd_state(&dut, mcp_port).await,
        "serial" => cmd_serial(&dut, mcp_port).await,
        "reboot" => cmd_reboot(&dut, mcp_port).await,
        "uboot" => cmd_uboot(&dut, mcp_port).await,
        "maskrom" => cmd_maskrom(&dut, mcp_port).await,
        "uf" => {
            if !positional.is_empty() {
                cmd_flash(&dut, &positional[0], "full", mcp_port).await
            } else {
                eprintln!("Usage: dutabo uf <image>");
            }
        }
        "flash-kernel" => {
            if !positional.is_empty() {
                cmd_flash(&dut, &positional[0], "kernel", mcp_port).await
            } else {
                eprintln!("Usage: dutabo flash-kernel <image>");
            }
        }
        _ => {
            eprintln!("Unknown command: {cmd}");
            print_usage();
        }
    }
}

fn print_usage() {
    eprintln!("\
dutabo — DUT control CLI for embedded Linux targets

Usage:
  dutabo <command> [options]

Commands:
  list                       List all configured DUTs from .target.toml
  state     [--dut <alias>]  Show DUT state (active/booting/crashed/...)
  serial    [--dut <alias>]  Interactive serial console (Ctrl-T q to quit)
  reboot    [--dut <alias>]  Software reboot the DUT
  uboot     [--dut <alias>]  Enter U-Boot interactive prompt
  maskrom   [--dut <alias>]  Enter Rockchip MASKROM mode
  uf <image> [--dut <alias>] Flash full firmware image
  flash-kernel <image> [--dut <alias>]  Flash kernel/boot image

Options:
  --dut <alias>    Select which DUT (required for multi-DUT configs)
  --mcp-port <N>   MCP HTTP port (default: 3000)

Examples:
  dutabo list
  dutabo state --dut rk3576-pdstars
  dutabo serial --dut rk3576-yt9215
  dutabo uf /path/to/update.img --dut rk3576-pdstars
  dutabo flash-kernel /path/to/boot.img
    ");
}

fn find_target_toml() -> Option<PathBuf> {
    let mut d = std::env::current_dir().ok()?;
    loop {
        let c = d.join(".target.toml");
        if c.exists() {
            return Some(c);
        }
        if !d.pop() {
            break;
        }
    }
    None
}

/// Deterministic MCP port (3001-3099), same algorithm as session-start.py.
fn project_mcp_port(project_dir: &std::path::Path) -> u16 {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    let canonical = std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    h.update(canonical.to_string_lossy().as_bytes());
    let hex = format!("{:x}", h.finalize());
    let val = u64::from_str_radix(&hex[..8], 16).unwrap_or(0);
    MC_PORT_BASE + (val % MC_PORT_RANGE as u64) as u16
}

/// Walk up from CWD to find `.mcp.json` and extract port from `debug-console.url`.
fn detect_mcp_port_from_config() -> Option<u16> {
    let mut d = std::env::current_dir().ok()?;
    loop {
        let mcp_json = d.join(".mcp.json");
        if mcp_json.exists() {
            if let Ok(content) = std::fs::read_to_string(&mcp_json) {
                if let Ok(cfg) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(url) = cfg["mcpServers"]["debug-console"]["url"].as_str() {
                        if let Some(port_str) = url.rsplit(':').next() {
                            if let Some(port) = port_str.split('/').next() {
                                if let Ok(p) = port.parse::<u16>() {
                                    return Some(p);
                                }
                            }
                        }
                    }
                }
            }
            return None;
        }
        if !d.pop() { break; }
    }
    None
}

fn select_dut(
    duts: &[debug_console_mcp::config::DutConfig],
    alias: Option<String>,
) -> Option<debug_console_mcp::config::DutConfig> {
    if duts.is_empty() {
        eprintln!("No DUTs.");
        return None;
    }
    if let Some(ref a) = alias {
        for d in duts {
            if d.alias == *a {
                return Some(d.clone());
            }
        }
        eprintln!("DUT '{a}' not found. Available:");
        for d in duts {
            eprintln!("  - {}", d.alias);
        }
        return None;
    }
    if duts.len() == 1 {
        return Some(duts[0].clone());
    }
    eprintln!("Multiple DUTs. Choose with --dut:");
    for d in duts {
        eprintln!("  - {}", d.alias);
    }
    None
}

fn cmd_list(duts: &[debug_console_mcp::config::DutConfig]) {
    for d in duts {
        println!("DUT: {}", d.alias);
        println!(
            "  dev_host: {} ({}@{})",
            d.dev_host_alias, d.dev_host_user, d.dev_host_ip
        );
        println!("  serial:   {}:{}", d.serial_ip, d.serial_port);
        println!(
            "  relay:    type={}, ip={}, port={}, reset_ch={}, maskrom_ch={}, recovery_ch={}, power_ch={}, power_off_ms={}",
            d.relay_type, d.relay_ip, d.relay_port, d.reset_ch, d.maskrom_ch, d.recovery_ch, d.power_ch, d.power_off_time_ms
        );
        println!("  login:    {}", d.login_user);
        println!("  ref_log:  {}", d.reference_log);
        println!();
    }
}

async fn mcp_call(
    dut: &debug_console_mcp::config::DutConfig,
    method: &str,
    args: serde_json::Value,
    port: u16,
) -> Option<serde_json::Value> {
    let url = format!("http://127.0.0.1:{port}/mcp");
    let client = reqwest::Client::new();
    'outer: for attempt in 0..2 {
        if !INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = client.post(&url).json(&serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"dutabo","version":"0.2.0"}}})).timeout(std::time::Duration::from_secs(5)).send().await;
            let _ = client
                .post(&url)
                .json(&serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await;
            INITIALIZED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":args});
        match client
            .post(&url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
        {
            Ok(resp) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    return json.get("result").cloned();
                }
            }
            Err(_) if attempt == 0 => {
                eprintln!("MCP server not running — starting...");
                let bin = std::env::current_exe()
                    .ok()
                    .and_then(|p| Some(p.parent()?.join("debug-console-mcp")))
                    .unwrap_or_else(|| PathBuf::from("debug-console-mcp"));
                let mut command = std::process::Command::new(&bin);
                command.args(["--http", &format!("127.0.0.1:{port}")]);
                command.env("TARGET_DUT_ALIAS", &dut.alias);
                if let Some(path) = find_target_toml() {
                    command.env("TARGET_CONF", path);
                }
                if command
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .is_ok()
                {
                    for _ in 0..10 {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        if std::process::Command::new("curl")
                            .args([
                                "-sf",
                                "-o",
                                "/dev/null",
                                &format!("http://127.0.0.1:{port}/health"),
                            ])
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false)
                        {
                            INITIALIZED.store(false, std::sync::atomic::Ordering::Relaxed);
                            continue 'outer;
                        }
                    }
                }
            }
            Err(_) => {}
        }
        break;
    }
    None
}

// ── Subcommands ──────────────────────────────────────────────────────────

async fn cmd_state(dut: &debug_console_mcp::config::DutConfig, port: u16) {
    let args = serde_json::json!({"name":"serial_get_state","arguments":{}});
    if let Some(r) = mcp_call(dut, "tools/call", args, port).await {
        if let Some(t) = r["content"][0]["text"].as_str() {
            if let Ok(s) = serde_json::from_str::<serde_json::Value>(t) {
                println!("State:     {}", s["state"].as_str().unwrap_or("unknown"));
                println!("Boot #:    {}", s["boot_number"]);
                println!(
                    "Last data: {:.1}s ago",
                    s["last_data_seconds"].as_f64().unwrap_or(0.0)
                );
                println!("Log path:  {}", s["log_path"].as_str().unwrap_or(""));
                println!(
                    "Relay:     {}",
                    if s["relay_configured"].as_bool().unwrap_or(false) {
                        "configured"
                    } else {
                        "not configured"
                    }
                );
                println!(
                    "Login:     {}",
                    if s["login_configured"].as_bool().unwrap_or(false) {
                        "configured"
                    } else {
                        "not configured"
                    }
                );
            }
        }
    }
}

async fn cmd_reboot(dut: &debug_console_mcp::config::DutConfig, port: u16) {
    // Priority: power cycle > relay reset > software reboot
    if dut.power_ch > 0 {
        println!("Power cycling (channel {}, {}ms off)...", dut.power_ch, dut.power_off_time_ms);
        let args = serde_json::json!({"name":"serial_power_cycle","arguments":{"wait_boot":true}});
        if let Some(r) = mcp_call(dut, "tools/call", args, port).await {
            println!("{r}");
        }
    } else if dut.reset_ch > 0 {
        println!("Relay reset (channel {})...", dut.reset_ch);
        let args = serde_json::json!({"name":"serial_reset","arguments":{"wait_boot":true}});
        if let Some(r) = mcp_call(dut, "tools/call", args, port).await {
            println!("{r}");
        }
    } else {
        println!("Software reboot via serial...");
        let args = serde_json::json!({"name":"serial_send_command","arguments":{"command":"reboot","timeout":5}});
        if let Some(r) = mcp_call(dut, "tools/call", args, port).await {
            println!("{r}");
        }
    }
}

async fn cmd_uboot(dut: &debug_console_mcp::config::DutConfig, port: u16) {
    let args = serde_json::json!({"name":"serial_enter_uboot","arguments":{}});
    if let Some(r) = mcp_call(dut, "tools/call", args, port).await {
        let t = r["content"][0]["text"].as_str().unwrap_or("");
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            if v["success"].as_bool().unwrap_or(false) {
                println!("{v}");
                return;
            }
            if v["error"]
                .as_str()
                .map(|s| s.contains("No relay"))
                .unwrap_or(false)
                || dut.reset_ch == 0
            {
                eprintln!("Relay not configured, trying software reboot...");
                let sw = serde_json::json!({"name":"serial_reboot_uboot","arguments":{}});
                if let Some(r2) = mcp_call(dut, "tools/call", sw, port).await {
                    println!("{r2}");
                }
                return;
            }
            println!("{v}");
        }
    }
}

async fn cmd_maskrom(dut: &debug_console_mcp::config::DutConfig, port: u16) {
    let args = serde_json::json!({"name":"serial_enter_maskrom","arguments":{}});
    if let Some(r) = mcp_call(dut, "tools/call", args, port).await {
        println!("{r}");
    }
}

// ── Serial (interactive) ─────────────────────────────────────────────────

async fn cmd_serial(_dut: &debug_console_mcp::config::DutConfig, mcp_port: u16) {
    // Ensure MCP server is running (HTTP required for WS)
    if !std::process::Command::new("curl")
        .args(["-sf", "-o", "/dev/null", &format!("http://127.0.0.1:{mcp_port}/health")])
        .status().map(|s| s.success()).unwrap_or(false)
    {
        eprintln!("Starting MCP server...");
        let bin = std::env::current_exe()
            .ok().and_then(|p| Some(p.parent()?.join("debug-console-mcp")))
            .unwrap_or_else(|| PathBuf::from("debug-console-mcp"));
        let mut command = std::process::Command::new(&bin);
        command.args(["--http", &format!("127.0.0.1:{mcp_port}")]);
        if let Some(path) = find_target_toml() {
            command.env("TARGET_CONF", path);
        }
        let _ = command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if std::process::Command::new("curl")
                .args(["-sf", "-o", "/dev/null", &format!("http://127.0.0.1:{mcp_port}/health")])
                .status().map(|s| s.success()).unwrap_or(false)
            { break; }
        }
    }

    serial_ws_relay(mcp_port).await;
    eprintln!("Session ended.");
}

// ── WebSocket serial relay (via MCP /serial/ws) ─────────────────────────

async fn serial_ws_relay(mcp_port: u16) {
    use std::io::Write;
    use futures_util::StreamExt;
    use tokio::io::AsyncReadExt;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    let url = format!("ws://127.0.0.1:{mcp_port}/serial/ws");
    let (ws, _) = match connect_async(&url).await {
        Ok(ws) => ws,
        Err(e) => { eprintln!("WS connect failed: {e}"); return; }
    };
    let (ws_sink, mut ws_stream) = ws.split();

    // Raw mode
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    let is_tty = unsafe { libc::isatty(0) != 0 };
    if is_tty {
        unsafe { libc::tcgetattr(0, &mut saved); }
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw); }
        raw.c_cc[libc::VERASE] = 0x7f;
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw); }
        eprintln!("Connected via MCP (Ctrl-T q to exit)\n");
    } else {
        eprintln!("Connected via MCP (non-interactive, 30s timeout)\n");
    }

    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r = running.clone();

    // stdin → WebSocket
    if is_tty {
        use futures_util::SinkExt;
        let mut sink = ws_sink;
        tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 1];
            let mut esc = false;
            loop {
                if !r.load(std::sync::atomic::Ordering::Relaxed) { break; }
                if stdin.read_exact(&mut buf).await.is_err() { break; }
                let b = buf[0];
                if esc {
                    if b == b'q' || b == b'Q' { r.store(false, std::sync::atomic::Ordering::Relaxed); break; }
                    let _ = sink.send(Message::Binary(vec![0x14, b].into())).await;
                    esc = false;
                } else if b == 0x14 { esc = true; }
                else { let _ = sink.send(Message::Text(String::from_utf8_lossy(&[b]).to_string().into())).await; }
            }
            r.store(false, std::sync::atomic::Ordering::Relaxed);
        });
    } else {
        let r2 = running.clone();
        tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_secs(30)).await; r2.store(false, std::sync::atomic::Ordering::Relaxed); });
    }

    // WebSocket → stdout (with ANSI filter)
    let mut out = Vec::with_capacity(4096);
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        match ws_stream.next().await {
            Some(Ok(Message::Binary(data))) => {
                out.clear();
                let raw = &data;
                let mut i = 0;
                while i < raw.len() {
                    if raw[i] == 0x1b && i + 1 < raw.len() {
                        let next = raw[i + 1];
                        if next == b'[' || next == b']' || next == b'(' || next == b')' || next == b'>' || next == b'=' {
                            i += 2;
                            while i < raw.len() && !(raw[i] >= b'@' && raw[i] <= b'~') && raw[i] != b'\x07' { i += 1; }
                            if i < raw.len() { i += 1; }
                            continue;
                        }
                    }
                    out.push(raw[i]); i += 1;
                }
                if !out.is_empty() { let _ = std::io::stdout().write_all(&out); let _ = std::io::stdout().flush(); }
            }
            Some(Ok(Message::Close(_))) | None => break,
            Some(Ok(_)) => {}
            Some(Err(e)) => { eprintln!("WS error: {e}"); break; }
        }
    }
    running.store(false, std::sync::atomic::Ordering::Relaxed);
    if is_tty { unsafe { libc::tcsetattr(0, libc::TCSANOW, &saved); } }
}

#[allow(dead_code)]
async fn cmd_serial_direct(dut: &debug_console_mcp::config::DutConfig, mcp_port: u16) {
    let host = if dut.serial_ip.is_empty() { &dut.dev_host_ip } else { &dut.serial_ip };

    if !std::process::Command::new("curl")
        .args(["-sf", "-o", "/dev/null", &format!("http://127.0.0.1:{mcp_port}/health")])
        .status().map(|s| s.success()).unwrap_or(false)
    {
        eprintln!("Starting MCP server...");
        let bin = std::env::current_exe()
            .ok().and_then(|p| Some(p.parent()?.join("debug-console-mcp")))
            .unwrap_or_else(|| PathBuf::from("debug-console-mcp"));
        let mut command = std::process::Command::new(&bin);
        command.args(["--http", &format!("127.0.0.1:{mcp_port}")]);
        command.env("TARGET_DUT_ALIAS", &dut.alias);
        if let Some(path) = find_target_toml() { command.env("TARGET_CONF", path); }
        let _ = command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if std::process::Command::new("curl")
                .args(["-sf", "-o", "/dev/null", &format!("http://127.0.0.1:{mcp_port}/health")])
                .status().map(|s| s.success()).unwrap_or(false)
            { break; }
        }
    }

    let sentinel = std::env::current_dir().unwrap_or_default().join(".dut-serial").join(".dutabo-active");
    std::fs::write(&sentinel, "1").ok();
    std::thread::sleep(std::time::Duration::from_millis(500));
    serial_tcp_relay(host, &dut.serial_port);
    std::fs::remove_file(&sentinel).ok();
    let _ = resume_agent(mcp_port);
    eprintln!("Session ended.");
}

#[allow(dead_code)]
#[allow(dead_code)]
fn pause_agent(port: u16) {
    let _ = std::process::Command::new("curl")
        .args(["-sf","-X","POST",&format!("http://127.0.0.1:{port}/mcp"),
            "-H","Content-Type: application/json",
            "-d",r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"dutabo","version":"0.2.0"}}}"#])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    let _ = std::process::Command::new("curl")
        .args([
            "-sf",
            "-X",
            "POST",
            &format!("http://127.0.0.1:{port}/mcp"),
            "-H",
            "Content-Type: application/json",
            "-d",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let _ = std::process::Command::new("curl")
        .args(["-sf","-X","POST",&format!("http://127.0.0.1:{port}/mcp"),
            "-H","Content-Type: application/json",
            "-d",r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"serial_pause","arguments":{}}}"#])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
}

#[allow(dead_code)]
#[allow(dead_code)]
fn resume_agent(port: u16) {
    let _ = std::process::Command::new("curl")
        .args(["-sf","-X","POST",&format!("http://127.0.0.1:{port}/mcp"),
            "-H","Content-Type: application/json",
            "-d",r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"dutabo","version":"0.2.0"}}}"#])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    let _ = std::process::Command::new("curl")
        .args([
            "-sf",
            "-X",
            "POST",
            &format!("http://127.0.0.1:{port}/mcp"),
            "-H",
            "Content-Type: application/json",
            "-d",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let _ = std::process::Command::new("curl")
        .args(["-sf","-X","POST",&format!("http://127.0.0.1:{port}/mcp"),
            "-H","Content-Type: application/json",
            "-d",r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"serial_resume","arguments":{}}}"#])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
}

// ── Serial TCP relay (pure Rust, no external tools) ──────────────────────

fn serial_tcp_relay(host: &str, port: &str) {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    // Save terminal settings, set raw mode
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    let is_tty = unsafe { libc::isatty(0) != 0 };
    if is_tty {
        unsafe {
            libc::tcgetattr(0, &mut saved);
        }
        let mut raw = saved;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        raw.c_cc[libc::VERASE] = 0x7f;
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &raw);
        }
    }

    // Connect TCP
    let addr = format!("{host}:{port}");
    let tcp = match TcpStream::connect(&addr) {
        Ok(s) => {
            s.set_nodelay(true).ok();
            s.set_nonblocking(true).ok();
            s
        }
        Err(e) => {
            eprintln!("Connect failed: {e}");
            if is_tty {
                unsafe {
                    libc::tcsetattr(0, libc::TCSANOW, &saved);
                }
            }
            return;
        }
    };
    let mut tcp_w = tcp.try_clone().unwrap();
    let mut tcp_r = tcp;
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r1 = running.clone();

    // Thread 1: stdin → TCP (with Ctrl-T q escape).
    // Only spawn when stdin is a real TTY. Otherwise (pipe/Agent),
    // just relay TCP→stdout — don't read stdin (immediate EOF).
    if is_tty {
        std::thread::spawn(move || {
            let mut buf = [0u8; 1];
            let mut esc = false;
            loop {
                if !r1.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                if std::io::stdin().lock().read_exact(&mut buf).is_err() {
                    break;
                }
                let b = buf[0];
                if esc {
                    if b == b'q' || b == b'Q' {
                        r1.store(false, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                    let _ = tcp_w.write_all(&[0x14]);
                    let _ = tcp_w.write_all(&[b]);
                    esc = false;
                } else if b == 0x14 {
                    esc = true;
                } else {
                    let _ = tcp_w.write_all(&[b]);
                }
            }
            r1.store(false, std::sync::atomic::Ordering::Relaxed);
        });
    } else {
        // Non-interactive mode: set a 30s timeout for output-only relay
        let r2 = running.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(30));
            r2.store(false, std::sync::atomic::Ordering::Relaxed);
        });
    }

    // Thread 2: TCP → stdout (with ANSI escape filtering)
    if is_tty {
        eprintln!("Connected to {host}:{port} (Ctrl-T q to exit)\n");
    } else {
        eprintln!("Connected to {host}:{port} (non-interactive, 30s timeout)\n");
    }
    let mut buf = [0u8; 4096];
    let mut out = Vec::with_capacity(4096);
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        match tcp_r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.clear();
                let raw = &buf[..n];
                let mut i = 0;
                while i < raw.len() {
                    // Skip ANSI escape sequences: ESC[ ... m/; / etc, ESC]0;...BEL
                    if raw[i] == 0x1b && i + 1 < raw.len() {
                        let next = raw[i + 1];
                        if next == b'[' || next == b']' || next == b'(' || next == b')'
                           || next == b'>' || next == b'='
                        {
                            // Skip ESC + bracket + parameters + terminator
                            i += 2;
                            while i < raw.len() && !(raw[i] >= b'@' && raw[i] <= b'~')
                                  && raw[i] != b'\x07'
                            {
                                i += 1;
                            }
                            if i < raw.len() {
                                i += 1; // skip terminator
                            }
                            continue;
                        }
                    }
                    out.push(raw[i]);
                    i += 1;
                }
                if !out.is_empty() {
                    let _ = std::io::stdout().write_all(&out);
                    let _ = std::io::stdout().flush();
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
    running.store(false, std::sync::atomic::Ordering::Relaxed);

    // Restore terminal
    if is_tty {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &saved);
        }
    }
}

async fn cmd_flash(
    dut: &debug_console_mcp::config::DutConfig,
    image_path: &str,
    image_type: &str,
    mcp_port: u16,
) {
    let real = std::fs::canonicalize(image_path).unwrap_or_else(|_| PathBuf::from(image_path));
    if !real.exists() {
        eprintln!("Image not found: {}", real.display());
        return;
    }
    println!(
        "Image: {} ({} bytes)",
        real.display(),
        std::fs::metadata(&real).map(|m| m.len()).unwrap_or(0)
    );

    let plan = match mcp_call(dut, "tools/call", serde_json::json!({"name":"serial_flash_plan","arguments":{"image_path":real.to_string_lossy(),"image_type":image_type}}), mcp_port).await {
        Some(r) => serde_json::from_str::<serde_json::Value>(r["content"][0]["text"].as_str().unwrap_or("{}")).unwrap_or_default(),
        None => { eprintln!("MCP server not reachable"); return; }
    };
    if !plan["success"].as_bool().unwrap_or(false) {
        eprintln!("{}", plan["error"].as_str().unwrap_or("unknown"));
        return;
    }

    let dh = plan["dev_host"].as_str().unwrap_or("");
    let du = plan["dev_user"].as_str().unwrap_or("");
    let rp = plan["remote_path"].as_str().unwrap_or("");
    let tool = plan["tool"].as_str().unwrap_or("");
    let fcmd = plan["selected_flash_cmd"].as_str().unwrap_or_else(|| {
        if image_type == "kernel" {
            plan["kernel_image_cmd"].as_str().unwrap_or("")
        } else {
            plan["full_image_cmd"].as_str().unwrap_or("")
        }
    });
    let lb = plan["loader_bin"].as_str().unwrap_or("");
    let list_devices_cmd = plan["list_devices_cmd"].as_str().unwrap_or("LD");
    let loader_cmd = plan["loader_cmd"].as_str().unwrap_or("");

    if dh.is_empty() {
        eprintln!("dev_host not configured");
        return;
    }

    // Upload
    println!("\n[1/3] Uploading to {du}@{dh}:{rp} ...");
    let scp_dest = format!("{du}@{dh}:{rp}");
    let ssh_dest = format!("{du}@{dh}");
    let real_str = real.to_string_lossy().to_string();
    if !std::process::Command::new("scp")
        .arg(&real_str)
        .arg(&scp_dest)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        eprintln!("SCP failed");
        return;
    }

    // Verify
    println!("\n[2/3] Verifying sha256 ...");
    let lsha = String::from_utf8_lossy(
        &std::process::Command::new("sha256sum")
            .arg(&real)
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default(),
    )
    .split_whitespace()
    .next()
    .unwrap_or("")
    .to_string();
    let rsha = String::from_utf8_lossy(
        &std::process::Command::new("ssh")
            .args([ssh_dest.as_str(), "sha256sum", rp])
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default(),
    )
    .split_whitespace()
    .next()
    .unwrap_or("")
    .to_string();
    if lsha != rsha || lsha.is_empty() {
        eprintln!("SHA256 MISMATCH: local={lsha} remote={rsha}");
        return;
    }
    println!("  OK");

    // Flash — auto-enter Loader mode if needed
    println!("\n[3/3] Flashing ...");
    let ld_cmd = format!("{tool} {list_devices_cmd}");

    // Check if device is connected, enter Loader mode if not
    let devs_out = std::process::Command::new("ssh")
        .args([ssh_dest.as_str(), ld_cmd.as_str()])
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    let devs = String::from_utf8_lossy(&devs_out).to_string();

    if devs.contains("connected(0)") || devs.contains("No found") {
        println!("  No device in Loader mode — sending 'reboot loader'...");
        // Use MCP to reboot target into Loader mode
        let _ = mcp_call(
            dut,
            "tools/call",
            serde_json::json!({
                "name": "serial_send_raw", "arguments": {"data": "reboot loader\n"}
            }),
            mcp_port,
        )
        .await;
        println!("  Waiting for device...");
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let check = std::process::Command::new("ssh")
                .args([ssh_dest.as_str(), ld_cmd.as_str()])
                .output()
                .map(|o| o.stdout)
                .unwrap_or_default();
            let check_str = String::from_utf8_lossy(&check);
            if !check_str.contains("connected(0)") && check_str.len() > 10 {
                break;
            }
        }
    }

    // Re-check device status
    let devs_out = std::process::Command::new("ssh")
        .args([ssh_dest.as_str(), ld_cmd.as_str()])
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    let devs = String::from_utf8_lossy(&devs_out).to_string();
    let maskrom = devs.contains("Maskrom") || devs.contains("MASKROM");

    if maskrom && !lb.is_empty() {
        println!("  MASKROM detected → flashing loader: {lb}");
        let loader_full_cmd = format!("{tool} {loader_cmd}");
        std::process::Command::new("ssh")
            .args([ssh_dest.as_str(), loader_full_cmd.as_str()])
            .status()
            .ok();
        std::thread::sleep(std::time::Duration::from_secs(5));
    } else if devs.contains("Loader") {
        println!("  Device in Loader mode — flashing...");
    } else {
        eprintln!("  Device not detected. Ensure USB is connected and try again.");
        return;
    }

    let flash_cmd = format!("{tool} {fcmd}");
    let st = std::process::Command::new("ssh")
        .args([ssh_dest.as_str(), flash_cmd.as_str()])
        .status();
    match st {
        Ok(s) if s.success() => println!("  Done!"),
        Ok(s) => eprintln!("  Failed (exit {})", s),
        Err(e) => eprintln!("  Error: {e}"),
    }
}
