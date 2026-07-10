//! dutabo — DUT control CLI for embedded Linux targets.
//!
//! Architecture:
//!   dutabo serial → pause Agent → WebSocket/TCP relay → resume Agent
//!   dutabo state/reboot/etc → MCP HTTP → Agent

use std::ops::Range;
use std::path::PathBuf;
use std::sync::LazyLock;

const MCP_DEFAULT_PORT: u16 = 3000;
const MC_PORT_BASE: u16 = 3001;
const MC_PORT_RANGE: u16 = 99;

// Health-check polling: how often and how many times we probe the MCP server
// after spawning it.  200ms × 15 = 3s total budget.
const HEALTH_POLL_INTERVAL_MS: u64 = 200;
const HEALTH_POLL_RETRIES: u32 = 15;

static INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct TerminalSanitizer {
    state: AnsiState,
    allow_color: bool,
}

enum AnsiState {
    Ground,
    Esc(Vec<u8>),
    Csi(Vec<u8>),
    Osc,
}

impl TerminalSanitizer {
    fn new(allow_color: bool) -> Self {
        Self {
            state: AnsiState::Ground,
            allow_color,
        }
    }

    fn filter(&mut self, raw: &[u8], out: &mut Vec<u8>) {
        for &b in raw {
            match &mut self.state {
                AnsiState::Ground => {
                    if b == 0x1b {
                        self.state = AnsiState::Esc(vec![b]);
                    } else {
                        out.push(b);
                    }
                }
                AnsiState::Esc(seq) => {
                    seq.push(b);
                    match b {
                        b'[' => {
                            let seq = std::mem::take(seq);
                            self.state = AnsiState::Csi(seq);
                        }
                        b']' => {
                            self.state = AnsiState::Osc;
                        }
                        b'(' | b')' | b'>' | b'=' => {}
                        _ => {
                            self.state = AnsiState::Ground;
                        }
                    }
                }
                AnsiState::Csi(seq) => {
                    seq.push(b);
                    if (b'@'..=b'~').contains(&b) {
                        if b == b'm' {
                            // SGR color code — strip (or preserve if allow_color + safe)
                            if self.allow_color && is_safe_sgr(seq) {
                                out.extend_from_slice(seq);
                            }
                        } else {
                            // Cursor movement, erase, insert/delete, save/restore —
                            // readline needs all non-SGR CSI sequences for correct
                            // visual feedback.  Stripping any of them causes cursor
                            // desync and backspace-deletes-wrong-character bugs.
                            out.extend_from_slice(seq);
                        }
                        self.state = AnsiState::Ground;
                    }
                }
                AnsiState::Osc => {
                    if b == b'\x07' {
                        self.state = AnsiState::Ground;
                    } else if b == b'\\' {
                        self.state = AnsiState::Ground;
                    }
                }
            }
        }
    }
}

fn is_safe_sgr(seq: &[u8]) -> bool {
    seq.starts_with(b"\x1b[")
        && seq[2..seq.len().saturating_sub(1)]
            .iter()
            .all(|b| b.is_ascii_digit() || *b == b';')
}


fn highlight_serial_prompt(data: &[u8], out: &mut Vec<u8>) {
    let mut start = 0;
    while start < data.len() {
        let end = data[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(data.len());
        highlight_serial_prompt_line(&data[start..end], out);
        start = end;
    }
}

struct PromptParts {
    prefix: Option<Range<usize>>,
    user: Range<usize>,
    separator: Option<Range<usize>>,
    dir: Range<usize>,
    suffix: Option<Range<usize>>,
    sigil: Range<usize>,
}

fn highlight_serial_prompt_line(line: &[u8], out: &mut Vec<u8>) {
    let Some(parts) = prompt_parts(line) else {
        out.extend_from_slice(line);
        return;
    };
    let tail = parts.sigil.end;

    if let Some(prefix) = parts.prefix {
        out.extend_from_slice(&line[prefix]);
    }
    out.extend_from_slice(b"\x1b[1;32m");
    out.extend_from_slice(&line[parts.user]);
    out.extend_from_slice(b"\x1b[0m");
    if let Some(separator) = parts.separator {
        out.extend_from_slice(&line[separator]);
    }
    out.extend_from_slice(b"\x1b[1;34m");
    out.extend_from_slice(&line[parts.dir]);
    out.extend_from_slice(b"\x1b[0m");
    if let Some(suffix) = parts.suffix {
        out.extend_from_slice(&line[suffix]);
    }
    out.extend_from_slice(b"\x1b[1;37m");
    out.extend_from_slice(&line[parts.sigil]);
    out.extend_from_slice(b"\x1b[0m");
    out.extend_from_slice(&line[tail..]);
}

fn prompt_parts(line: &[u8]) -> Option<PromptParts> {
    if line.first().is_none_or(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let end = line
        .iter()
        .position(|&b| b == b'\r' || b == b'\n')
        .unwrap_or(line.len());
    let text = std::str::from_utf8(&line[..end]).ok()?;

    parse_colon_prompt(text, line).or_else(|| parse_bracket_prompt(text, line))
}

fn parse_colon_prompt(text: &str, line: &[u8]) -> Option<PromptParts> {
    static COLON_USER_RE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
        fancy_regex::Regex::new(r"^[^/#\-\s]\S*\s?[^\s.:\[\]]+?(?=:\S[^\r\n$#]*[$#]\s)").unwrap()
    });
    static COLON_DIR_RE: LazyLock<fancy_regex::Regex> =
        LazyLock::new(|| fancy_regex::Regex::new(r"(?<=:)\S[^\r\n$#]*?(?=[$#]\s)").unwrap());
    static COLON_SIGIL_RE: LazyLock<fancy_regex::Regex> =
        LazyLock::new(|| fancy_regex::Regex::new(r"(?<=\S)[$#]\s").unwrap());
    let user = match_range(&COLON_USER_RE, text)?;
    let dir = match_range(&COLON_DIR_RE, text)?;
    let sigil = match_range(&COLON_SIGIL_RE, text)?;
    let separator = user.end..dir.start;
    if &line[separator.clone()] != b":" {
        return None;
    }
    if !valid_prompt_user(&line[user.clone()]) || !valid_prompt_dir(&line[dir.clone()]) {
        return None;
    }
    Some(PromptParts {
        prefix: None,
        user,
        separator: Some(separator),
        dir,
        suffix: None,
        sigil,
    })
}

fn parse_bracket_prompt(text: &str, line: &[u8]) -> Option<PromptParts> {
    static BRACKET_USER_RE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
        fancy_regex::Regex::new(r"(?<=^\[)[^/#\-\s]\S*\s?[^\s.\[\]]+?(?=\s+\S[^\[\]\r\n]*\][$#]\s)")
            .unwrap()
    });
    static BRACKET_DIR_RE: LazyLock<fancy_regex::Regex> =
        LazyLock::new(|| fancy_regex::Regex::new(r"(?<=\s)\S[^\]\r\n]*?(?=\][$#]\s)").unwrap());
    static BRACKET_SIGIL_RE: LazyLock<fancy_regex::Regex> =
        LazyLock::new(|| fancy_regex::Regex::new(r"(?<=\])[$#]\s").unwrap());
    let user = match_range(&BRACKET_USER_RE, text)?;
    let dir = match_range(&BRACKET_DIR_RE, text)?;
    let sigil = match_range(&BRACKET_SIGIL_RE, text)?;
    if text.as_bytes().first() != Some(&b'[') || dir.end >= sigil.start {
        return None;
    }
    let prefix = 0..1;
    let separator = user.end..dir.start;
    let suffix = dir.end..sigil.start;
    if !line[separator.clone()]
        .iter()
        .all(|b| b.is_ascii_whitespace())
        || &line[suffix.clone()] != b"]"
    {
        return None;
    }
    if !valid_prompt_user(&line[user.clone()]) || !valid_prompt_dir(&line[dir.clone()]) {
        return None;
    }
    Some(PromptParts {
        prefix: Some(prefix),
        user,
        separator: Some(separator),
        dir,
        suffix: Some(suffix),
        sigil,
    })
}

fn match_range(re: &fancy_regex::Regex, text: &str) -> Option<Range<usize>> {
    re.find(text).ok().flatten().map(|m| m.start()..m.end())
}

fn valid_prompt_user(user: &[u8]) -> bool {
    !user.is_empty()
        && !matches!(user[0], b'/' | b'#' | b'-' | b' ' | b'\t')
        && user
            .iter()
            .all(|b| !b.is_ascii_control() && !matches!(*b, b':' | b'[' | b']' | b'/' | b'#'))
        && user.iter().any(|b| b.is_ascii_alphanumeric())
}

fn valid_prompt_dir(dir: &[u8]) -> bool {
    !dir.is_empty()
        && !dir[0].is_ascii_whitespace()
        && dir
            .iter()
            .all(|b| !b.is_ascii_control() && !matches!(*b, b'[' | b']'))
}

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
    eprintln!(
        "\
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
    "
    );
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
    let canonical =
        std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
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
            "  relay:    type={}, ip={}, port={}, reset_ch={}, maskrom_ch={}, recovery_ch={}, power_ch={}, power_off_ms={}",
            d.relay_type,
            d.relay_ip,
            d.relay_port,
            d.reset_ch,
            d.maskrom_ch,
            d.recovery_ch,
            d.power_ch,
            d.power_off_time_ms
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
    let Some(r) = mcp_call(dut, "tools/call", args, port).await else {
        eprintln!("MCP server not reachable on port {port}");
        return;
    };
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

async fn cmd_reboot(dut: &debug_console_mcp::config::DutConfig, port: u16) {
    // Priority: power cycle > relay reset > software reboot
    if dut.power_ch > 0 {
        println!(
            "Power cycling (channel {}, {}ms off)...",
            dut.power_ch, dut.power_off_time_ms
        );
        let args = serde_json::json!({"name":"serial_power_cycle","arguments":{"wait_boot":true}});
        if let Some(r) = mcp_call(dut, "tools/call", args, port).await {
            println!("{r}");
        } else {
            eprintln!("MCP server not reachable on port {port}");
        }
    } else if dut.reset_ch > 0 {
        println!("Relay reset (channel {})...", dut.reset_ch);
        let args = serde_json::json!({"name":"serial_reset","arguments":{"wait_boot":true}});
        if let Some(r) = mcp_call(dut, "tools/call", args, port).await {
            println!("{r}");
        } else {
            eprintln!("MCP server not reachable on port {port}");
        }
    } else {
        println!("Software reboot via serial...");
        let args = serde_json::json!({"name":"serial_send_command","arguments":{"command":"reboot","timeout":5}});
        if let Some(r) = mcp_call(dut, "tools/call", args, port).await {
            println!("{r}");
        } else {
            eprintln!("MCP server not reachable on port {port}");
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
                } else {
                    eprintln!("MCP server not reachable on port {port}");
                }
                return;
            }
            println!("{v}");
            return;
        }
    }
    eprintln!("MCP server not reachable on port {port}");
}

async fn cmd_maskrom(dut: &debug_console_mcp::config::DutConfig, port: u16) {
    let args = serde_json::json!({"name":"serial_enter_maskrom","arguments":{}});
    if let Some(r) = mcp_call(dut, "tools/call", args, port).await {
        println!("{r}");
    } else {
        eprintln!("MCP server not reachable on port {port}");
    }
}

// ── Serial (interactive) ─────────────────────────────────────────────────

async fn cmd_serial(dut: &debug_console_mcp::config::DutConfig, mcp_port: u16) {
    // Ensure MCP server is running (HTTP required for WS)
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
        if let Some(path) = find_target_toml() {
            command.env("TARGET_CONF", path);
        }
        // Pass DUT alias so the MCP uses the correct serial config
        command.env("TARGET_DUT_ALIAS", &dut.alias);
        let _ = command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        for _ in 0..HEALTH_POLL_RETRIES {
            std::thread::sleep(std::time::Duration::from_millis(HEALTH_POLL_INTERVAL_MS));
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

    serial_ws_relay(mcp_port).await;
    // Session ended — clean exit, no message needed.
}

// ── WebSocket serial relay (via MCP /serial/ws) ─────────────────────────

async fn serial_ws_relay(mcp_port: u16) {
    use futures_util::StreamExt;
    use std::io::Write;
    use tokio::io::AsyncReadExt;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    let url = format!("ws://127.0.0.1:{mcp_port}/serial/ws");
    let (ws, _) = match connect_async(&url).await {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("WS connect failed: {e}");
            return;
        }
    };
    let (ws_sink, mut ws_stream) = ws.split();

    // Raw mode
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
        eprintln!("Connected via MCP (Ctrl-T q to exit)\n");
    } else {
        eprintln!("Connected via MCP (non-interactive, 30s timeout)\n");
    }

    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

    // stdin → WebSocket
    if is_tty {
        use futures_util::SinkExt;
        let mut sink = ws_sink;
        let r = running.clone();
        tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 1];
            let mut esc = false;
            loop {
                if !r.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                if stdin.read_exact(&mut buf).await.is_err() {
                    break;
                }
                let b = buf[0];
                if esc {
                    if b == b'q' || b == b'Q' {
                        r.store(false, std::sync::atomic::Ordering::Relaxed);
                        // Send close frame to unblock the main loop —
                        // ws_stream.next() won't return until the server
                        // closes its side or we close ours.
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                    let _ = sink.send(Message::Binary(vec![0x14, b].into())).await;
                    esc = false;
                } else if b == 0x14 {
                    esc = true;
                } else {
                    let _ = sink
                        .send(Message::Text(
                            String::from_utf8_lossy(&[b]).to_string().into(),
                        ))
                        .await;
                }
            }
            r.store(false, std::sync::atomic::Ordering::Relaxed);
            // Ensure the main loop wakes up even on non-esc exit (stdin EOF/error).
            let _ = sink.send(Message::Close(None)).await;
        });
    } else {
        let r = running.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            r.store(false, std::sync::atomic::Ordering::Relaxed);
        });
    }

    // WebSocket → stdout.
    // TerminalSanitizer strips SGR color codes from kernel boot logs.
    // A small readline CSI subset (cursor movement A/B/C/D, erase-to-EOL K,
    // and delete-character P) passes through so the shell can move the cursor
    // and redraw the command line without allowing screen-wide controls from
    // serial logs.
    // Prompt highlighting then re-adds safe SGR colors for readability.
    let mut sanitizer = TerminalSanitizer::new(is_tty);
    let mut out = Vec::with_capacity(4096);
    let mut rendered = Vec::with_capacity(4096);
    let mut first_write = true;
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        match ws_stream.next().await {
            Some(Ok(Message::Binary(data))) => {
                out.clear();
                sanitizer.filter(&data, &mut out);
                if !out.is_empty() {
                    // Ensure the first byte written to stdout starts at column 0.
                    // The server may have stale serial data that reaches us mid-line.
                    if first_write {
                        first_write = false;
                        if out.first() != Some(&b'\r') && out.first() != Some(&b'\n') {
                            let _ = std::io::stdout().write_all(b"\r\n");
                        }
                    }
                    rendered.clear();
                    if is_tty {
                        highlight_serial_prompt(&out, &mut rendered);
                    } else {
                        rendered.extend_from_slice(&out);
                    }
                    let _ = std::io::stdout().write_all(&rendered);
                    let _ = std::io::stdout().flush();
                }
            }
            Some(Ok(Message::Close(_))) | None => break,
            Some(Ok(_)) => {}
            Some(Err(e)) => {
                // Suppress noise on clean disconnect — the user pressed Ctrl-T q
                // or the server shut down normally. Only log unexpected errors.
                let msg = e.to_string().to_lowercase();
                if !msg.contains("reset without closing")
                    && !msg.contains("connection reset")
                    && !msg.contains("broken pipe")
                    && !msg.contains("connection closed")
                {
                    eprintln!("WS error: {e}");
                }
                break;
            }
        }
    }
    running.store(false, std::sync::atomic::Ordering::Relaxed);
    if is_tty {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &saved);
        }
    }
}

#[allow(dead_code)]
async fn cmd_serial_direct(dut: &debug_console_mcp::config::DutConfig, mcp_port: u16) {
    let host = if dut.serial_ip.is_empty() {
        &dut.dev_host_ip
    } else {
        &dut.serial_ip
    };

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
        for _ in 0..HEALTH_POLL_RETRIES {
            std::thread::sleep(std::time::Duration::from_millis(HEALTH_POLL_INTERVAL_MS));
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

    // Use the same project directory as the MCP (where .target.toml lives),
    // not current_dir(), so the sentinel is found by the engine's read loop.
    let project_dir = find_target_toml()
        .as_ref()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let sentinel = project_dir.join(".dut-serial").join(".dutabo-active");
    std::fs::write(&sentinel, "1").ok();
    std::thread::sleep(std::time::Duration::from_millis(500));
    serial_tcp_relay(host, &dut.serial_port);
    std::fs::remove_file(&sentinel).ok();
    let _ = resume_agent(mcp_port);
    // Session ended — clean exit, no message needed.
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

    // Thread 2: TCP → stdout.
    // TerminalSanitizer strips SGR color codes; a small readline CSI subset
    // passes through for prompt redraw and in-line deletion support.
    // Prompt highlighting re-adds safe SGR colors for readability.
    if is_tty {
        eprintln!("Connected to {host}:{port} (Ctrl-T q to exit)\n");
    } else {
        eprintln!("Connected to {host}:{port} (non-interactive, 30s timeout)\n");
    }
    let mut buf = [0u8; 4096];
    let mut sanitizer = TerminalSanitizer::new(is_tty);
    let mut out = Vec::with_capacity(4096);
    let mut rendered = Vec::with_capacity(4096);
    while running.load(std::sync::atomic::Ordering::Relaxed) {
        match tcp_r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.clear();
                sanitizer.filter(&buf[..n], &mut out);
                if !out.is_empty() {
                    rendered.clear();
                    if is_tty {
                        highlight_serial_prompt(&out, &mut rendered);
                    } else {
                        rendered.extend_from_slice(&out);
                    }
                    let _ = std::io::stdout().write_all(&rendered);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Result of feeding a byte into the Ctrl-T escape detector.
    #[derive(Debug, PartialEq, Eq)]
    enum EscapeAction {
        Send(Vec<u8>),
        None,
        Quit,
    }

    struct CtrlTEscape {
        esc: bool,
    }

    impl CtrlTEscape {
        fn new() -> Self {
            Self { esc: false }
        }

        fn feed(&mut self, b: u8) -> EscapeAction {
            if self.esc {
                self.esc = false;
                if b == b'q' || b == b'Q' {
                    return EscapeAction::Quit;
                }
                return EscapeAction::Send(vec![0x14, b]);
            }
            if b == 0x14 {
                self.esc = true;
                return EscapeAction::None;
            }
            EscapeAction::Send(vec![b])
        }
    }

    #[test]
    fn test_ctrl_t_q_quits() {
        let mut esc = CtrlTEscape::new();
        assert_eq!(esc.feed(0x14), EscapeAction::None); // Ctrl-T
        assert_eq!(esc.feed(b'q'), EscapeAction::Quit); // q → quit
    }

    #[test]
    fn test_ctrl_t_capital_q_quits() {
        let mut esc = CtrlTEscape::new();
        assert_eq!(esc.feed(0x14), EscapeAction::None);
        assert_eq!(esc.feed(b'Q'), EscapeAction::Quit);
    }

    #[test]
    fn test_ctrl_t_other_key_passthrough() {
        let mut esc = CtrlTEscape::new();
        assert_eq!(esc.feed(0x14), EscapeAction::None); // Ctrl-T
        // Pressing 'x' after Ctrl-T → both bytes sent
        assert_eq!(esc.feed(b'x'), EscapeAction::Send(vec![0x14, b'x']));
    }

    #[test]
    fn test_double_ctrl_t() {
        let mut esc = CtrlTEscape::new();
        assert_eq!(esc.feed(0x14), EscapeAction::None); // first Ctrl-T
        assert_eq!(esc.feed(0x14), EscapeAction::Send(vec![0x14, 0x14])); // second sent through
    }

    #[test]
    fn test_normal_keys_passthrough() {
        let mut esc = CtrlTEscape::new();
        assert_eq!(esc.feed(b'a'), EscapeAction::Send(vec![b'a']));
        assert_eq!(esc.feed(b'\n'), EscapeAction::Send(vec![b'\n']));
        assert_eq!(esc.feed(0x03), EscapeAction::Send(vec![0x03])); // Ctrl-C
    }

    #[test]
    fn test_esc_resets_after_quit() {
        // After a quit sequence, a new machine should be clean.
        // Verify that esc flag doesn't persist between sequences.
        let mut esc = CtrlTEscape::new();
        assert_eq!(esc.feed(0x14), EscapeAction::None);
        assert_eq!(esc.feed(b'q'), EscapeAction::Quit);
        // esc flag is now false (reset by feed)
        assert!(!esc.esc);
        // Next byte after quit is a normal key
        assert_eq!(esc.feed(b'a'), EscapeAction::Send(vec![b'a']));
    }

    #[test]
    fn sanitizer_preserves_split_sgr_without_leaking_final_byte() {
        let mut sanitizer = TerminalSanitizer::new(true);
        let mut out = Vec::new();

        sanitizer.filter(b"2: eth0: <BROADCAST> mtu 1500 state \x1b[32", &mut out);
        sanitizer.filter(b"mUP\x1b[0", &mut out);
        sanitizer.filter(b"m group default\n    inet \x1b[32", &mut out);
        sanitizer.filter(b"m127.0.0.1\x1b[0", &mut out);
        sanitizer.filter(b"m/8 scope host lo\n", &mut out);

        let rendered = String::from_utf8(out).unwrap();
        assert_eq!(
            rendered,
            "2: eth0: <BROADCAST> mtu 1500 state \x1b[32mUP\x1b[0m group default\n    inet \x1b[32m127.0.0.1\x1b[0m/8 scope host lo\n"
        );
        assert!(!rendered.contains("state mUP"));
        assert!(!rendered.contains("inet m127"));
    }

    #[test]
    fn sanitizer_strips_non_color_control_sequences() {
        let mut sanitizer = TerminalSanitizer::new(true);
        let mut out = Vec::new();

        sanitizer.filter(b"a\x1b[2Jb\x1b]0;title\x07c", &mut out);

        assert_eq!(String::from_utf8(out).unwrap(), "abc");
    }

    #[test]
    fn sanitizer_preserves_readline_redraw_sequences() {
        let mut sanitizer = TerminalSanitizer::new(true);
        let mut out = Vec::new();

        sanitizer.filter(b"abc\x1b[D\x1b[Kd\x1b[P", &mut out);

        assert_eq!(String::from_utf8(out).unwrap(), "abc\x1b[D\x1b[Kd\x1b[P");
    }

    #[test]
    fn prompt_highlight_colors_user_dir_and_sigil() {
        let mut out = Vec::new();

        highlight_serial_prompt(b"root@myd-lt527:/# ip -c a\r\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();

        assert!(rendered.contains("\x1b[1;32mroot@myd-lt527\x1b[0m:"));
        assert!(rendered.contains("\x1b[1;34m/\x1b[0m"));
        assert!(rendered.contains("\x1b[1;37m# \x1b[0mip -c a"));
    }

    #[test]
    fn prompt_highlight_handles_root_without_host() {
        let mut out = Vec::new();

        highlight_serial_prompt(b"root:/# ls\r\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();

        assert!(rendered.contains("\x1b[1;32mroot\x1b[0m:"));
        assert!(rendered.contains("\x1b[1;34m/\x1b[0m"));
        assert!(rendered.contains("\x1b[1;37m# \x1b[0mls"));
    }

    #[test]
    fn prompt_highlight_handles_bracket_prompt() {
        let mut out = Vec::new();

        highlight_serial_prompt(b"[root@myd-lt527 ~]# pwd\r\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();

        assert!(rendered.contains("[\x1b[1;32mroot@myd-lt527\x1b[0m "));
        assert!(rendered.contains("\x1b[1;34m~\x1b[0m]"));
        assert!(rendered.contains("\x1b[1;37m# \x1b[0mpwd"));
    }

    #[test]
    fn prompt_highlight_handles_user_shell_prompt() {
        let mut out = Vec::new();

        highlight_serial_prompt(b"ubuntu@board:~/work tree$ git status\r\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();

        assert!(rendered.contains("\x1b[1;32mubuntu@board\x1b[0m:"));
        assert!(rendered.contains("\x1b[1;34m~/work tree\x1b[0m"));
        assert!(rendered.contains("\x1b[1;37m$ \x1b[0mgit status"));
    }

    #[test]
    fn prompt_highlight_handles_bracket_user_without_host() {
        let mut out = Vec::new();

        highlight_serial_prompt(b"[root /var/log]# tail messages\r\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();

        assert!(rendered.contains("[\x1b[1;32mroot\x1b[0m "));
        assert!(rendered.contains("\x1b[1;34m/var/log\x1b[0m]"));
        assert!(rendered.contains("\x1b[1;37m# \x1b[0mtail messages"));
    }

    #[test]
    fn prompt_highlight_does_not_color_non_prompt_lines() {
        let mut out = Vec::new();

        highlight_serial_prompt(b"cat: /tmp# not a prompt\r\n", &mut out);

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "cat: /tmp# not a prompt\r\n"
        );
    }
}
