//! Embedded Debug MCP Server — Rust 实现
//!
//! 通过 Dev Host ser2net 连接嵌入式 Linux 目标板的 MCP 串口调试工具。
//! 使用 TCP 直连 ser2net (socket:// 协议)，无 socat，无 SSH。
//!
//! Protocol: MCP (Model Context Protocol) 2024-11-05
//! Transport: stdio (newline-delimited JSON-RPC 2.0)

mod boot_detector;
mod command_queue;
mod config;
mod console;
mod lock_manager;
mod log_manager;
mod marker;
mod mcp;
mod mcp_http;
mod relay_manager;
mod serial_engine;
mod state_manager;

const HELP: &str = "\
embedded-debug-mcp — MCP serial console debugger for embedded Linux targets

Usage:
  embedded-debug-mcp [OPTIONS]
  embedded-debug-mcp --help
  embedded-debug-mcp --version

Description:
  An MCP (Model Context Protocol) server that connects to embedded Linux
  target boards via Dev Host ser2net. Provides per-power-cycle log capture,
  automatic U-Boot interrupt, boot-completion login, kernel crash detection,
  and relay reset control.

  Default transport is stdio (JSON-RPC 2.0 newline-delimited).
  Use --http to expose as a Streamable HTTP server (SSE deprecated per MCP 2025 spec).

  Supports: tools, resources (log files), prompts (debug templates).

Options:
  -h, --help         Show this help message and exit.
  -V, --version      Print version and exit.
  -v, --verbose      Increase log verbosity (debug level on stderr).
                     Default: info level to {project}/.dut-serial/mcp.log.
      --log-to-stderr  Log to stderr instead of file (useful for debugging).
      --http [HOST:PORT]  Run as Streamable HTTP server (default: 0.0.0.0:3000).

Environment:
  TARGET_CONF        Path to .target.conf file (alternative to CWD search).
  RUST_LOG           tracing filter (e.g. RUST_LOG=debug).
                     Overridden by --verbose.

Configuration:
  The server searches recursively upward from CWD for .target.conf.
  See README.md for the full configuration reference.

  Example .target.conf:
    RK_DEV_HOST_IP=192.168.1.189
    RK_SERIAL_PORT=2000
    RK_LOGIN_USER=root
    RK_LOGIN_PASS=mypassword
    RK_RELAY_PORT=2001
    RK_RESET_CHANNEL=2
    RK_MASKROM_CHANNEL=1

MCP Tools:
  serial_send_command  - Execute shell command on target
  serial_get_state     - Get target state and metadata
  serial_get_logs      - Retrieve serial logs with pattern filtering
  serial_list_logs     - List archived boot logs
  serial_reset         - Hardware reset via relay + log rotation
  serial_enter_uboot   - Force target into U-Boot prompt
  serial_wait_pattern  - Wait for regex pattern in serial output
  serial_new_log       - Manually rotate log without reset
  serial_poll_logs     - Get new serial output since last poll
  serial_get_config    - Show current target configuration

Files:
  {project}/.target.conf          Target configuration
  {project}/.dut-serial/logs/     Boot log archives
  {project}/.dut-serial/target-state   Current state file
  {project}/.dut-serial/mcp.log   Server log
  /tmp/embedded-debug/locks/      Per host:port mutual exclusion

Version: embedded-debug-mcp v{}\n";

fn print_help() {
    let msg = HELP.replace("{}", env!("CARGO_PKG_VERSION"));
    eprintln!("{msg}");
}

fn print_version() {
    eprintln!("embedded-debug-mcp v{}", env!("CARGO_PKG_VERSION"));
}

#[tokio::main]
async fn main() {
    // ── CLI 参数解析 ──
    let mut verbose = false;
    let mut log_to_stderr = false;
    let mut http_mode = false;
    let mut http_bind = "0.0.0.0:3000".to_string();

    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                print_version();
                std::process::exit(0);
            }
            "-v" | "--verbose" => verbose = true,
            "--log-to-stderr" => log_to_stderr = true,
            "--http" => {
                http_mode = true;
                // Check if next arg is a HOST:PORT (doesn't start with -)
                if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    i += 1;
                    http_bind = args[i].clone();
                }
            }
            other => {
                eprintln!("Unknown option: {other}");
                eprintln!("Use --help for usage information.");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    // ── 初始化日志 ──
    let log_level = if verbose { "debug" } else { "info" };

    if log_to_stderr {
        // 日志写到 stderr (调试用)
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive(
                        format!("embedded_debug_mcp={log_level}").parse().unwrap(),
                    ),
            )
            .init();
    } else {
        // 日志写到文件 (stdout 留给 JSON-RPC)
        let log_dir = std::path::PathBuf::from(
            config::load_config()
                .project_dir
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string()),
        )
        .join(".dut-serial");
        std::fs::create_dir_all(&log_dir).ok();
        let log_file = log_dir.join("mcp.log");

        tracing_subscriber::fmt()
            .with_writer(move || {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_file)
                    .unwrap_or_else(|_| panic!("Cannot open log file: {log_file:?}"))
            })
            .with_ansi(false)
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive(
                        format!("embedded_debug_mcp={log_level}").parse().unwrap(),
                    ),
            )
            .init();
    }

    tracing::info!("embedded-debug-mcp v{} starting", env!("CARGO_PKG_VERSION"));

    // ── 加载配置 ──
    let cfg = config::load_config();
    if cfg.config_path.is_none() {
        tracing::error!(
            "No .target.conf found. cwd={}, TARGET_CONF={:?}",
            std::env::current_dir().unwrap_or_default().display(),
            std::env::var("TARGET_CONF").ok()
        );
    }

    // ── 创建并启动 engine（带超时防护）──
    let engine = serial_engine::new_shared_engine(cfg.clone());

    {
        let mut eng = engine.lock().await;
        match tokio::time::timeout(std::time::Duration::from_secs(5), eng.start()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!("Engine start failed: {e}"),
            Err(_) => tracing::error!("Engine start timed out after 5s"),
        }

        // 自动加载参考日志 (RK_REFERENCE_LOG in .target.conf)
        let ref_log = cfg.reference_log();
        if !ref_log.is_empty() {
            let path = std::path::PathBuf::from(&ref_log);
            match eng.detector.load_reference(&path) {
                Ok(()) => tracing::info!("Auto-loaded reference log: {ref_log}"),
                Err(e) => tracing::warn!("Failed to load reference log '{ref_log}': {e}"),
            }
        }
    }

    // ── 运行 MCP server ──
    if http_mode {
        // Streamable HTTP transport (2025-03-26 spec)
        let (host, port) = if let Some((h, p)) = http_bind.rsplit_once(':') {
            (h.to_string(), p.parse::<u16>().unwrap_or(3000))
        } else {
            ("0.0.0.0".to_string(), 3000u16)
        };
        tracing::info!("Starting Streamable HTTP on {host}:{port}");
        if let Err(e) = mcp_http::run_http(engine.clone(), &host, port).await {
            tracing::error!("HTTP server error: {e}");
        }
    } else {
        // Stdio transport (default)
        let mut server = mcp::McpServer::new(engine.clone());
        if let Err(e) = server.run().await {
            tracing::error!("MCP server error: {e}");
        }
    }

    // ── 停止 engine ──
    {
        let mut eng = engine.lock().await;
        eng.stop().await;
    }

    tracing::info!("embedded-debug-mcp stopped");
}
