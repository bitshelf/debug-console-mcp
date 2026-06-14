//! Serial port simulator — mimics a Linux development board's serial console.
//!
//! Listens on a TCP port (like ser2net) and behaves like a login shell:
//! - Boot banner + login prompt
//! - Shell prompt with hostname
//! - Echoes typed characters
//! - Responds to built-in commands (echo, ls, cat, dmesg, etc.)
//! - Can inject fake kernel boot logs
//!
//! ## Usage
//!
//! ```bash
//! # Basic: login shell on port 9999
//! serial-sim
//!
//! # With boot log replay + login shell on port 9999
//! serial-sim --boot-log
//!
//! # Custom port and hostname
//! serial-sim --port 2000 --hostname myd-lt527
//!
//! # Raw mode: no login, just shell prompt (for testing dutabo serial)
//! serial-sim --no-login
//! ```
//!
//! ## Test with dutabo
//!
//! ```bash
//! # Terminal 1
//! serial-sim --port 2000
//!
//! # Terminal 2 — config points to localhost:2000
//! dutabo serial
//! ```

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_PORT: u16 = 9999;

// ── Fake filesystem ────────────────────────────────────────────────────────

struct FakeFS {
    files: HashMap<String, String>,
}

impl FakeFS {
    fn new(hostname: &str) -> Self {
        let mut files = HashMap::new();
        files.insert(
            "/proc/version".into(),
            "Linux version 5.15.147 (tao@tao-virtual-machine) \
             (aarch64-none-linux-gnu-gcc 10.3.1) #29 SMP PREEMPT \
             Thu Nov 13 17:22:05 CST 2025\n"
                .into(),
        );
        files.insert(
            "/proc/cpuinfo".into(),
            "processor\t: 0\nBogoMIPS\t: 48.00\nFeatures\t: fp asimd evtstrm aes \
             pmull sha1 sha2 crc32\nCPU implementer\t: 0x41\nCPU architecture: 8\n\
             CPU variant\t: 0x2\nCPU part\t: 0xd05\nCPU revision\t: 0\n"
                .into(),
        );
        files.insert(
            "/proc/meminfo".into(),
            "MemTotal:        1924996 kB\nMemFree:         1500000 kB\n\
             MemAvailable:    1700000 kB\nBuffers:           50000 kB\n\
             Cached:           100000 kB\n"
                .into(),
        );
        files.insert(
            "/etc/hostname".into(),
            format!("{hostname}\n"),
        );
        Self { files }
    }

    fn read(&self, path: &str) -> Option<&str> {
        self.files.get(path).map(|s| s.as_str())
    }
}

// ── Boot log generator ─────────────────────────────────────────────────────

const BOOT_LOG: &str = r#"[    0.000000] Booting Linux on physical CPU 0x0000000000 [0x412fd050]
[    0.000000] Linux version 5.15.147 (tao@tao-virtual-machine) (aarch64-none-linux-gnu-gcc (GNU Toolchain for the A-profile Architecture 10.3-2021.07 (arm-10.29)) 10.3.1 20210621, GNU ld (GNU Toolchain for the A-profile Architecture 10.3-2021.07 (arm-10.29)) 2.36.1.20210621) #29 SMP PREEMPT Thu Nov 13 17:22:05 CST 2025
[    0.000000] Machine model: sun55iw3
[    0.000000] Zone ranges:
[    0.000000]   DMA      [mem 0x0000000040000000-0x00000000bfffffff]
[    0.000000] psci: PSCIv1.1 detected in firmware.
[    0.068248] Detected VIPT I-cache on CPU4
[    0.070044] smp: Brought up 1 node, 8 CPUs
[    0.206583] SMP: Total of 8 processors activated.
[    0.762519] sunxi:ccu_sunxi_ng:[INFO]: rtc_ccu: sunxi ccu init OK
[    2.142047] iommu: Default domain type: Translated
[    2.154284] SCSI subsystem initialized
[    2.891909] sunxi:sunxi_mmc_host-4022000.sdmmc:[INFO]: SD/MMC/SDIO Host Controller Driver(v5.48 2024-07-17 16:19)
[    3.330066] mmcblk0: mmc0:0001 8GUF4R 7.28 GiB
[    3.333140]  mmcblk0: p1 p2 p3 p4
[    3.333767] mmcblk0rpmb: mmc0:0001 8GUF4R 4.00 MiB, chardev (241:0)
[    4.329162] mmc0: new HS200 MMC card at address 0001
[    5.477194] xhci-hcd xhci-hcd.23.auto: xHCI Host Controller
[    6.280812] In-situ OAM (IOAM) with IPv6
[    8.281880] EXT4-fs (mmcblk0p4): mounted filesystem with ordered data mode.
[   10.045413] EXT4-fs (mmcblk0p4): re-mounted. Opts: (null). Quota mode: disabled.
[   11.000000] Welcome to Embedded Linux
"#;

// ── Shell simulator ────────────────────────────────────────────────────────

struct Shell {
    hostname: String,
    fs: FakeFS,
    boot_time: Instant,
    line_buf: String,
    echo: bool,
    logged_in: bool,
    login_user: String,
}

impl Shell {
    fn new(hostname: &str) -> Self {
        Self {
            hostname: hostname.to_string(),
            fs: FakeFS::new(hostname),
            boot_time: Instant::now(),
            line_buf: String::new(),
            echo: true,
            logged_in: false,
            login_user: String::new(),
        }
    }

    fn prompt(&self) -> String {
        if !self.logged_in {
            format!("\r\n{} login: ", self.hostname)
        } else {
            format!("\r\n{}@{}:/# ", self.login_user, self.hostname)
        }
    }

    fn initial_prompt(&self) -> String {
        if !self.logged_in {
            format!("{} login: ", self.hostname)
        } else {
            format!("{}@{}:/# ", self.login_user, self.hostname)
        }
    }

    /// Process a line of input, return output.
    fn process_line(&mut self, line: &str) -> String {
        let line = line.trim();
        if line.is_empty() {
            return self.prompt();
        }

        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        let cmd = parts[0];
        let arg = parts.get(1).unwrap_or(&"").trim();

        match cmd {
            // Login flow
            "login" | "root" if !self.logged_in => {
                self.logged_in = true;
                self.login_user = if cmd == "login" && !arg.is_empty() {
                    arg.to_string()
                } else {
                    "root".to_string()
                };
                format!("\r\nPassword: \r\n{}", self.prompt())
            }

            // Built-in commands
            "echo" => {
                format!("\r\n{}{}", arg, self.prompt())
            }
            "cat" => {
                if arg.is_empty() {
                    format!("\r\n{}", self.prompt())
                } else if let Some(content) = self.fs.read(arg) {
                    format!("\r\n{}{}", content, self.prompt())
                } else {
                    format!("\r\ncat: {}: No such file or directory{}", arg, self.prompt())
                }
            }
            "ls" => {
                let target = if arg.is_empty() { "/" } else { arg };
                let listing = match target {
                    "/" => "bin\r\ndev\r\netc\r\nproc\r\ntmp\r\nusr\r\nvar",
                    "/proc" => "cpuinfo\r\nmeminfo\r\nversion",
                    "/etc" => "hostname",
                    _ => "",
                };
                format!("\r\n{}{}", listing, self.prompt())
            }
            "dmesg" => {
                let lines: Vec<&str> = BOOT_LOG.lines().take(20).collect();
                format!("\r\n{}{}", lines.join("\r\n"), self.prompt())
            }
            "dmesg-full" => {
                format!("\r\n{}{}", BOOT_LOG.trim(), self.prompt())
            }
            "uptime" => {
                let secs = self.boot_time.elapsed().as_secs();
                let mins = secs / 60;
                let hours = mins / 60;
                format!(
                    "\r\n{:02}:{:02}:{:02} up {} min, load average: 0.00, 0.00, 0.00{}",
                    hours % 24,
                    mins % 60,
                    secs % 60,
                    mins,
                    self.prompt()
                )
            }
            "whoami" => {
                format!("\r\n{}{}", self.login_user, self.prompt())
            }
            "hostname" => {
                format!("\r\n{}{}", self.hostname, self.prompt())
            }
            "pwd" => {
                format!("\r\n/{}", self.prompt())
            }
            "uname" => {
                let out: String = match arg {
                    "-a" => format!("Linux {} 5.15.147 #29 SMP PREEMPT Thu Nov 13 17:22:05 CST 2025 aarch64 GNU/Linux", self.hostname),
                    "-r" => "5.15.147".into(),
                    _ => "Linux".into(),
                };
                format!("\r\n{}{}", out, self.prompt())
            }
            "stty" => {
                if arg.contains("echo") {
                    self.echo = !arg.contains("-echo");
                }
                self.prompt()
            }
            "false" => {
                format!("\r\n{}", self.prompt())
            }
            "true" => {
                self.prompt()
            }
            "warmup" | "echo warmup" => {
                // MCP engine warmup command — just return prompt
                self.prompt()
            }

            // Any other input → echo it if echo is on
            _ => {
                if !self.logged_in {
                    format!("\r\nLogin incorrect\r\n{}", self.prompt())
                } else {
                    // Unknown command: simulate "command not found"
                    format!(
                        "\r\n/bin/sh: {}: not found{}",
                        cmd,
                        self.prompt()
                    )
                }
            }
        }
    }
}

// ── Connection handler ─────────────────────────────────────────────────────

fn handle_client(
    mut stream: TcpStream,
    hostname: &str,
    with_boot_log: bool,
    no_login: bool,
    running: Arc<AtomicBool>,
) {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok();
    stream.set_nonblocking(false).ok();

    let mut shell = Shell::new(hostname);
    if no_login {
        shell.logged_in = true;
        shell.login_user = "root".to_string();
    }

    // Send boot log if requested
    if with_boot_log {
        // Simulate staggered boot log output
        let boot_lines: Vec<&str> = BOOT_LOG.lines().collect();
        for line in &boot_lines {
            if !running.load(Ordering::Relaxed) {
                return;
            }
            let _ = stream.write_all(format!("{}\r\n", line).as_bytes());
            let _ = stream.flush();
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // Send initial prompt
    let _ = stream.write_all(shell.initial_prompt().as_bytes());
    let _ = stream.flush();

    // Disable Nagle for responsive character echo
    let _ = stream.set_nodelay(true);

    let mut read_buf = [0u8; 256];

    while running.load(Ordering::Relaxed) {
        match stream.read(&mut read_buf) {
            Ok(0) => break, // connection closed
            Ok(n) => {
                let data = &read_buf[..n];

                // Handle backspace
                for &b in data {
                    match b {
                        b'\x7f' | b'\x08' => {
                            // Backspace
                            if !shell.line_buf.is_empty() {
                                shell.line_buf.pop();
                                if shell.echo {
                                    let _ = stream.write_all(b"\x08 \x08");
                                }
                            }
                        }
                        b'\r' | b'\n' => {
                            // Enter — process the line
                            if shell.echo {
                                let _ = stream.write_all(b"\r\n");
                            }
                            let line = std::mem::take(&mut shell.line_buf);
                            let response = shell.process_line(&line);
                            let _ = stream.write_all(response.as_bytes());
                            let _ = stream.flush();
                        }
                        b'\x04' => {
                            // Ctrl-D → exit
                            let _ = stream.write_all(b"\r\nlogout\r\n");
                            let _ = stream.flush();
                            shell.logged_in = false;
                            shell.login_user.clear();
                            let _ = stream.write_all(shell.initial_prompt().as_bytes());
                            let _ = stream.flush();
                        }
                        b'\x03' => {
                            // Ctrl-C → new prompt
                            shell.line_buf.clear();
                            let _ = stream.write_all(b"^C");
                            let response = shell.prompt();
                            let _ = stream.write_all(response.as_bytes());
                            let _ = stream.flush();
                        }
                        _ if b.is_ascii_graphic() || b == b' ' || b == b'\t' => {
                            shell.line_buf.push(b as char);
                            if shell.echo {
                                let _ = stream.write_all(&[b]);
                                let _ = stream.flush();
                            }
                        }
                        _ => {
                            // Pass through other control chars (e.g. ANSI sequences)
                            if shell.echo {
                                let _ = stream.write_all(&[b]);
                            }
                        }
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(_) => break,
        }
    }
}

// ── Main ───────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Derive hostname from system, with --hostname flag to override.
    let sys_hostname = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "embedded".to_string());

    let mut port = DEFAULT_PORT;
    let mut hostname = sys_hostname;
    let mut with_boot_log = false;
    let mut no_login = false;
    let mut print_port = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--port" | "-p" => {
                i += 1;
                if i < args.len() {
                    port = args[i].parse().unwrap_or(DEFAULT_PORT);
                }
            }
            "--hostname" | "-h" => {
                i += 1;
                if i < args.len() {
                    hostname = args[i].clone();
                }
            }
            "--boot-log" | "-b" => with_boot_log = true,
            "--no-login" | "-n" => no_login = true,
            "--print-port" | "-P" => print_port = true,
            "--help" => {
                println!(
                    "serial-sim — Fake embedded Linux serial console\n\
                     \nUsage: serial-sim [OPTIONS]\n\
                     \nOptions:\n  \
                     -p, --port PORT     TCP port to listen on (default: {DEFAULT_PORT})\n  \
                     -h, --hostname NAME Hostname for the simulated board (default: system hostname)\n  \
                     -b, --boot-log      Send fake kernel boot log on connect\n  \
                     -n, --no-login      Skip login prompt, go straight to shell\n  \
                     -P, --print-port    Print listening port on stdout\n  \
                     --help              Show this help\n\
                     \nCommands supported: echo, cat, ls, dmesg, uptime, whoami, \
                     hostname, pwd, uname, stty"
                );
                return;
            }
            _ => {
                eprintln!("Unknown flag: {}", args[i]);
                return;
            }
        }
        i += 1;
    }

    let addr = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            // Try with a random port
            eprintln!("Cannot bind to {addr}: {e}");
            std::process::exit(1);
        }
    };

    let actual_port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    eprintln!(
        "[serial-sim] Listening on 127.0.0.1:{actual_port} (hostname={hostname}, login={})\n",
        if no_login { "no" } else { "yes" }
    );

    if print_port {
        println!("{actual_port}");
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Graceful shutdown: wait for Enter on stdin (only if stdin is a terminal).
    // In test environments stdin is /dev/null or a pipe, so we skip this thread
    // and rely on SIGTERM/SIGINT for shutdown.
    let stdin_is_tty = unsafe { libc::isatty(0) != 0 };
    if stdin_is_tty {
        let _ = std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = std::io::stdin().read_line(&mut buf);
            r.store(false, Ordering::Relaxed);
        });
        eprintln!("[serial-sim] Press Enter to stop.");
    } else {
        eprintln!("[serial-sim] Running (non-interactive — send SIGTERM to stop).");
    }

    // Accept loop — single connection at a time (like real serial)
    for incoming in listener.incoming() {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        match incoming {
            Ok(stream) => {
                let peer = stream.peer_addr().unwrap();
                eprintln!("[serial-sim] Client connected: {peer}");
                handle_client(stream, &hostname, with_boot_log, no_login, running.clone());
                eprintln!("[serial-sim] Client disconnected: {peer}");
            }
            Err(e) => {
                eprintln!("[serial-sim] Accept error: {e}");
                break;
            }
        }
    }

    eprintln!("[serial-sim] Shutting down.");
}
