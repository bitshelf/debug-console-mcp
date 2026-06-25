//! dutabo — DUT control CLI for embedded Linux targets.
//!
//! Architecture:
//!   dutabo serial → pause Agent → nc ser2net → resume Agent
//!   dutabo state/reboot/etc → MCP HTTP → Agent

use std::path::PathBuf;

const MCP_DEFAULT_PORT: u16 = 3000;

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
    let mut cmd: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--dut" && i + 1 < args.len() {
            dut_alias = Some(args[i + 1].clone());
            i += 2;
        } else if args[i] == "--mcp-port" && i + 1 < args.len() {
            mcp_port = args[i + 1].parse().unwrap_or(MCP_DEFAULT_PORT);
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
            "  relay:    type={}, ip={}, port={}, reset_ch={}, maskrom_ch={}, recovery_ch={}",
            d.relay_type, d.relay_ip, d.relay_port, d.reset_ch, d.maskrom_ch, d.recovery_ch
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
    let args = serde_json::json!({"name":"serial_send_command","arguments":{"command":"reboot","timeout":5}});
    if let Some(r) = mcp_call(dut, "tools/call", args, port).await {
        println!("{r}");
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

async fn cmd_serial(dut: &debug_console_mcp::config::DutConfig, mcp_port: u16) {
    let host = if dut.serial_ip.is_empty() {
        &dut.dev_host_ip
    } else {
        &dut.serial_ip
    };

    // Ensure MCP server is running
    if !std::process::Command::new("curl")
        .args([
            "-sf",
            "-o",
            "/dev/null",
            &format!("http://127.0.0.1:{mcp_port}/health"),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        eprintln!("Starting MCP server...");
        let bin = std::env::current_exe()
            .ok()
            .and_then(|p| Some(p.parent()?.join("debug-console-mcp")))
            .unwrap_or_else(|| PathBuf::from("debug-console-mcp"));
        let mut command = std::process::Command::new(&bin);
        command.args(["--http", &format!("127.0.0.1:{mcp_port}")]);
        command.env("TARGET_DUT_ALIAS", &dut.alias);
        if let Some(path) = find_target_toml() {
            command.env("TARGET_CONF", path);
        }
        let _ = command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if std::process::Command::new("curl")
                .args([
                    "-sf",
                    "-o",
                    "/dev/null",
                    &format!("http://127.0.0.1:{mcp_port}/health"),
                ])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
            {
                break;
            }
        }
    }

    // Signal MCP to release serial (writes sentinel file)
    let sentinel = std::env::current_dir()
        .unwrap_or_default()
        .join(".dut-serial")
        .join(".dutabo-active");
    std::fs::write(&sentinel, "1").ok();
    // Wait for MCP to release
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Native Rust TCP serial relay — no external tools, no shell escaping
    serial_tcp_relay(host, &dut.serial_port);

    // Clean up sentinel so MCP can reconnect
    std::fs::remove_file(&sentinel).ok();
    let _ = resume_agent(mcp_port);
    eprintln!("Session ended.");
}

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
