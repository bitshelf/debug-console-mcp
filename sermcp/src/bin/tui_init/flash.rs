//! Flash-box probes for the `dutabo init` TUI (the TEST button's
//! device-list check): SSH `flash.list_devices_cmd` on the dev host BEFORE
//! and AFTER triggering flashing mode, and XOR the two outputs — the
//! newly-appeared device lines are the probe detail shown on the read-only
//! "flash.devices" row.
//!
//! The tool is NEVER hardcoded: whatever command the operator puts in
//! `flash.list_devices_cmd` (Rockchip `upgrade_tool ld`, Allwinner
//! `sunxi-fel list`, fastboot devices, rkdeveloptool, ...) is run
//! verbatim, and the before/after comparison is line-based with digit-run
//! normalization — count-bearing headers and re-enumeration number shifts
//! cancel, only genuinely new device lines survive. Other boards already
//! sitting in flashing mode appear in BOTH captures and cancel out, so the
//! probe is multi-DUT safe.
//!
//! After the trigger the probe polls the list for ~15 s: a Rockchip DUT
//! takes ~7 s to reboot into loader (DDR training) plus USB re-enumeration,
//! so a short poll window silently misses the device and false-fails.
//!
//! The trigger is CONTEXT-AWARE. `flash.loader_cmd` is a command sent on
//! the DUT's serial console, but the marker-echo wrapper only works in a
//! Linux shell, while U-Boot commands (`db {loader}`, `rockusb 0 mmc 0`)
//! only work AT the U-Boot prompt. The probe checks the DUT state first
//! (serial_get_state):
//!
//!  - state `uboot` — the U-Boot interrupt probe that ran earlier in the
//!    same TEST leaves the DUT AT the prompt. The loader_cmd is tried RAW
//!    there first (U-Boot commands need no shell wrapper; a shell command
//!    no-ops harmlessly as "Unknown command" and falls through). If no
//!    device appears, `boot` resumes the interrupted boot and the probe
//!    waits for the Linux shell before sending the loader_cmd shell-style.
//!  - state `booting`/`connecting` — the baud/relay probes reset the DUT
//!    earlier; the probe waits for the shell before triggering.
//!  - otherwise the loader_cmd is sent immediately (marker-wrapped).
//!
//! A serial trigger whose poll shows no new device falls THROUGH to the
//! recovery/maskrom relay attempts instead of failing immediately: the
//! marker echo completes even when the command no-ops (a shell command
//! rejected at the U-Boot prompt), so a "successful" serial call is not
//! proof the DUT entered flashing mode.

use super::{FieldKey, ProbeMark, ProbeTarget};

/// The read-only "flash.devices" row key (synthetic — never a config
/// field): the device-list probe's mark and detail land here.
pub fn devices_key(hi: usize, di: usize) -> FieldKey {
    FieldKey::DutAdvanced(hi, di, "flash.devices")
}

/// Is this the read-only flash device-list row?
pub fn is_devices_key(key: &FieldKey) -> bool {
    matches!(key, FieldKey::DutAdvanced(_, _, "flash.devices"))
}

/// The row label.
pub fn devices_label() -> &'static str {
    "flash.devices"
}

/// First line of a command's stderr (the actionable failure reason).
fn first_stderr_line(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .next()
        .unwrap_or("unknown ssh error")
        .trim()
        .to_string()
}

/// Did the key-auth attempt fail on AUTHENTICATION (as opposed to the
/// remote command itself exiting non-zero)? ssh prints "Permission
/// denied"/"publickey" on ITS stderr for auth failures; a remote
/// command's own non-zero exit (e.g. `sunxi-fel list` with zero devices)
/// leaves ssh's stderr empty.
fn ssh_auth_denied(out: &std::process::Output) -> bool {
    let err = String::from_utf8_lossy(&out.stderr).to_ascii_lowercase();
    err.contains("permission denied") || err.contains("publickey")
}

/// Upper bound for one ssh invocation. `ConnectTimeout` only bounds the TCP
/// connect — a remote command wedged on a hung USB stack would otherwise
/// park the probe thread forever and starve every later probe in its lane.
const SSH_COMMAND_TIMEOUT_SECS: u64 = 30;

/// Wait for a spawned child with a hard wall-clock cap: the child is killed
/// and reaped when it outlives `secs`. `None` means "capped".
fn wait_with_timeout(
    child: &mut std::process::Child,
    secs: u64,
) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
}

/// Drain a child pipe on a helper thread so a chatty child cannot fill the
/// 64K pipe buffer and deadlock against [`wait_with_timeout`]'s wait loop.
fn drain_pipe<R: std::io::Read + Send + 'static>(
    pipe: Option<R>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe
            && let Err(e) = pipe.read_to_end(&mut buf)
        {
            // EOF after the child dies arrives as Ok(0); anything else means
            // the pipe broke — the partial capture is still the best we have.
            let _ = e;
        }
        buf
    })
}

/// [`Command::output`](std::process::Command::output) with a hard cap:
/// stdout/stderr are drained on helper threads, the child is killed at the
/// deadline, and a capped run surfaces as `Err` (the caller's actionable
/// reason), never as a partial success.
fn run_capped(mut cmd: std::process::Command, secs: u64) -> Result<std::process::Output, String> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdout = drain_pipe(child.stdout.take());
    let stderr = drain_pipe(child.stderr.take());
    let status = wait_with_timeout(&mut child, secs);
    let out = std::process::Output {
        status: status
            .ok_or_else(|| format!("remote command did not finish within {secs}s (killed)"))?,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    };
    Ok(out)
}

/// One ssh invocation through the login chain (key auth first, then
/// sshpass with the password — the same chain as `probe_ssh_login`).
///
/// Returns `Ok(output)` whenever the remote command RAN (any exit code —
/// the caller interprets the exit status). `Err(reason)` only when ssh
/// itself could not start, the login chain was exhausted, or the command
/// outlived [`SSH_COMMAND_TIMEOUT_SECS`]; the reason is an ACTIONABLE hint
/// (missing key auth / sshpass, connection failure) — the classic cause is
/// "no ssh-copy-id yet", which has nothing to do with the DUT or the flash
/// tool.
fn ssh_output(
    host: &str,
    user: &str,
    pass: &str,
    cmd: &str,
) -> Result<std::process::Output, String> {
    use std::process::Command;
    let dest = format!("{user}@{host}");
    let base = [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "StrictHostKeyChecking=accept-new",
    ];
    // Key auth first. A non-zero exit is an auth failure only when ssh
    // itself says so on stderr — a remote command's own non-zero exit
    // (e.g. `sunxi-fel list` with zero devices) must NOT trigger the
    // password fallback.
    let first = run_capped(
        {
            let mut c = Command::new("setsid");
            c.arg("ssh").args(base).arg(&dest).arg(cmd);
            c
        },
        SSH_COMMAND_TIMEOUT_SECS,
    );
    let auth_denied = match &first {
        Ok(out) => !out.status.success() && ssh_auth_denied(out),
        Err(_) => true,
    };
    if !auth_denied {
        return first;
    }
    let key_err = match &first {
        Ok(out) => first_stderr_line(out),
        Err(e) => e.clone(),
    };
    if pass.is_empty() {
        return Err(format!(
            "{key_err} — run once on this machine: ssh-copy-id {dest}"
        ));
    }
    let sshpass_ok = Command::new("sshpass")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if sshpass_ok {
        let pwd_base = [
            "-o",
            "BatchMode=no",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "StrictHostKeyChecking=accept-new",
        ];
        return run_capped(
            {
                let mut c = Command::new("setsid");
                c.arg("sshpass")
                    .arg("-p")
                    .arg(pass)
                    .arg("ssh")
                    .args(pwd_base)
                    .arg("-o")
                    .arg("PreferredAuthentications=password")
                    .arg(&dest)
                    .arg(cmd);
                c
            },
            SSH_COMMAND_TIMEOUT_SECS,
        );
    }
    Err(format!(
        "{key_err} — and sshpass is not installed; run once on this machine: \
         ssh-copy-id {dest}"
    ))
}

/// The device-list capture used by the flash probe: non-zero remote exit
/// is an error only when stderr says something — the first stderr line
/// becomes the reason (ssh auth denial, tool errors like `upgrade_tool`'s
/// usb_claim_interface failure). A non-zero exit with EMPTY stderr is an
/// empty device list, not a failure: `sunxi-fel list` prints nothing and
/// exits 1 when zero Allwinner devices are in FEL mode, while
/// `upgrade_tool ld` prints "connected(0)" and exits 0. Login-chain
/// failures carry an ssh-copy-id hint (see [`ssh_output`]).
fn ssh_list_devices_stdout(
    host: &str,
    user: &str,
    pass: &str,
    cmd: &str,
) -> Result<String, String> {
    let out = ssh_output(host, user, pass, cmd)?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        // A help/usage dump is NOT a device list: bare `upgrade_tool` (no
        // subcommand) prints ~40 usage lines, which the XOR would happily
        // count as "N devices were ALREADY visible". Fail loudly with the
        // actionable diagnosis instead.
        if looks_like_usage(&stdout) {
            return Err(format!(
                "`{cmd}` printed a HELP/USAGE text ({} lines), not a device list — \
                 the command must LIST devices in flashing mode (e.g. `upgrade_tool ld`)",
                stdout.lines().count()
            ));
        }
        Ok(stdout)
    } else if !String::from_utf8_lossy(&out.stderr).trim().is_empty() {
        Err(first_stderr_line(&out))
    } else if stdout.trim().is_empty() {
        Ok(String::new())
    } else {
        Err(stdout
            .lines()
            .next()
            .unwrap_or("remote command failed")
            .trim()
            .to_string())
    }
}

pub fn preflight_list_devices(
    host: &str,
    user: &str,
    pass: &str,
    tool: &str,
    list_cmd: &str,
) -> (ProbeMark, Option<String>) {
    let list = format!("{} {}", tool.trim(), list_cmd.trim())
        .trim()
        .to_string();
    if list.is_empty() {
        return (ProbeMark::Skip, Some("list_devices_cmd is empty".into()));
    }
    match ssh_list_devices_stdout(host, user, pass, &list) {
        Ok(_) => (ProbeMark::Ok, None),
        Err(reason) => (
            ProbeMark::Err,
            Some(format!("list_devices_cmd over SSH failed: {reason}")),
        ),
    }
}

/// Heuristic for help/usage dumps: tool help screens open with a
/// usage/banner line ("Tool Usage", "usage: …", "Help:") — real device
/// listings never contain these.
fn looks_like_usage(stdout: &str) -> bool {
    stdout.contains("Usage") || stdout.contains("usage") || stdout.contains("Help:")
}

/// Cap for waiting on the DUT's serial state after resuming a boot from
/// the U-Boot prompt (or a mid-boot reset): a Rockchip board takes
/// roughly 20–40 s from power-on to the Linux shell, so 45 s covers slow
/// boards without stretching the TEST spinner into the next probe.
const BOOT_TO_SHELL_WAIT_SECS: u64 = 45;

/// Sleep between device-list polls (and state polls). The
/// `DUTABO_TEST_FAST_FLASH_POLL` env var shrinks it to ~0 so the
/// end-to-end probe tests do not wait out real poll windows — a test-only
/// seam following the house precedent (TARGET_CONF / DUTABO_AGENT_CATALOG
/// env overrides).
fn poll_interval() -> std::time::Duration {
    if std::env::var_os("DUTABO_TEST_FAST_FLASH_POLL").is_some() {
        std::time::Duration::from_millis(10)
    } else {
        std::time::Duration::from_secs(LIST_POLL_INTERVAL_SECS)
    }
}

/// Current DUT state via the MCP server (`serial_get_state`), or None
/// when no server answered or the payload was unusable.
pub(crate) fn mcp_dut_state(ports: &[u16]) -> Option<String> {
    mcp_dut_state_on(super::probe_runtime(), ports)
}

/// [`mcp_dut_state`] on a caller-supplied runtime: the poll loops below call
/// this up to several times a second against the same server, so they keep
/// one runtime for the whole loop instead of rebuilding per poll.
fn mcp_dut_state_on(rt: &tokio::runtime::Runtime, ports: &[u16]) -> Option<String> {
    let outcome = rt.block_on(super::super::mcp_call(
        "tools/call",
        serde_json::json!({"name": "serial_get_state", "arguments": {}}),
        ports,
    ));
    match outcome {
        super::super::McpOutcome::Result(result) => super::super::tool_payload(&result)
            .ok()
            .and_then(|payload| payload["state"].as_str().map(str::to_string)),
        _ => None,
    }
}

/// Poll `serial_get_state` until the DUT reaches the wanted state, or the
/// window runs out. Used to land the shell-style loader command in a LIVE
/// Linux console — the marker-echo wrapper only works in a shell.
fn wait_for_dut_state(ports: &[u16], wanted: &str, timeout_secs: u64) -> bool {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    while std::time::Instant::now() < deadline {
        if mcp_dut_state_on(&rt, ports).as_deref() == Some(wanted) {
            return true;
        }
        std::thread::sleep(poll_interval());
    }
    mcp_dut_state_on(&rt, ports).as_deref() == Some(wanted)
}

/// Release a held relay button, retrying because a stranded press boots the
/// DUT into recovery/maskrom on the next power-on. Returns true when any
/// attempt reached the server and succeeded.
fn release_button(ports: &[u16], button: &str) -> bool {
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        if super::mcp_probe_mark(
            ports,
            "serial_button",
            serde_json::json!({"button": button, "action": "release"}),
        ) == super::ProbeMark::Ok
        {
            return true;
        }
    }
    false
}

/// Normalize maximal digit runs to a single 'N' so that lines that only
/// differ in NUMBERS compare equal: list headers with a device count
/// (`List of rockusb connected(1)` vs `connected(2)`), DevNo /
/// LocationID / bus:dev numbers that shift on re-enumeration, sunxi-fel
/// SIDs. Fully tool-agnostic — no tool name is ever hardcoded, so any
/// command placed in `list_devices_cmd` (upgrade_tool, sunxi-fel,
/// fastboot, rkdeveloptool, ...) adapts automatically.
fn normalize_digit_runs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_digits = false;
    for ch in line.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push('N');
                in_digits = true;
            }
        } else {
            in_digits = false;
            out.push(ch);
        }
    }
    out
}

/// Line-level XOR of two command outputs: the lines of `new` that were NOT
/// in `old` (the device that appeared after the DUT entered flashing mode).
/// Blank lines are dropped.
///
/// Matching is a MULTISET diff on digit-normalized keys, not set
/// membership: a lab wall of same-model boards produces dozens of device
/// lines whose normalized forms are IDENTICAL (every differing digit run —
/// DevNo, serial — collapses to `N`), so one new device among 38 must be
/// detected by the COUNT growing from 38 to 39 for that key, never by
/// "this normalized line was seen before". Count-bearing headers
/// (`connected(1)` → `connected(2)`) stay quiet: their count grows by one
/// but the header line itself is boilerplate filtered downstream.
fn xor_new_lines(old: &str, new: &str) -> Vec<String> {
    use std::collections::{BTreeMap, BTreeSet};
    let clean = |l: &str| l.trim_end_matches('\r').trim().to_string();
    let old_raw: BTreeSet<String> = old.lines().map(clean).filter(|l| !l.is_empty()).collect();
    // Baseline grouped by normalized key → how many raw lines share it.
    let mut old_counts: BTreeMap<String, usize> = BTreeMap::new();
    for l in &old_raw {
        *old_counts.entry(normalize_digit_runs(l)).or_default() += 1;
    }
    // New side grouped in first-seen order, so reported additions preserve
    // the device list's own ordering.
    let mut new_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut group_order: Vec<String> = Vec::new();
    for l in new.lines().map(clean).filter(|l| !l.is_empty()) {
        let key = normalize_digit_runs(&l);
        let group = new_groups.entry(key.clone()).or_insert_with(|| {
            group_order.push(key.clone());
            Vec::new()
        });
        group.push(l);
    }
    let mut out = Vec::new();
    for key in group_order {
        let group = &new_groups[&key];
        let baseline = old_counts.get(&key).copied().unwrap_or(0);
        let added = group.len().saturating_sub(baseline);
        if added == 0 {
            continue;
        }
        // Prefer raw lines that are genuinely absent from the baseline;
        // fall back to the tail of the group when every raw form was seen
        // (pure re-enumeration shift of an existing device).
        let fresh: Vec<&String> = group.iter().filter(|l| !old_raw.contains(*l)).collect();
        let reported: Vec<&String> = if fresh.is_empty() {
            group.iter().rev().take(added).collect()
        } else {
            fresh.into_iter().take(added).collect()
        };
        out.extend(reported.into_iter().cloned());
    }
    out
}

/// List-header boilerplate (never a device), recognized generically:
/// upgrade_tool's `List of rockusb connected(N)`, fastboot's
/// `List of devices attached`, rkdeveloptool's `List of devices ...`.
fn is_list_boilerplate(line: &str) -> bool {
    line.contains("connected(") || line.starts_with("List of ")
}

/// Poll cadence after triggering flashing mode. A Rockchip DUT takes ~7 s
/// to reboot into loader (DDR training) plus USB re-enumeration — a short
/// window silently misses the device and false-fails; Allwinner FEL
/// appears in a few seconds. A 1 s cadence detects the device sooner and
/// still fits the TEST budget: 15 polls × 1 s = a 15 s window, covering
/// the ~7 s Rockchip loader entry with margin.
const LIST_POLLS: u64 = 15;
const LIST_POLL_INTERVAL_SECS: u64 = 1;

/// Outcome of the after-trigger polling loop.
enum PollOutcome {
    /// New device line(s) appeared — this DUT entered flashing mode.
    Added(Vec<String>),
    /// Every capture ran cleanly but no new line appeared.
    NoChange,
    /// The list command itself failed (ssh / tool error).
    Failed(String),
}

/// ③④ After a trigger: poll `list_devices_cmd` on the dev host until a
/// line NOT in `baseline` appears (this DUT's device), or the window runs
/// out. Other boards already in flashing mode are in both captures and
/// cancel out of the XOR.
fn poll_for_new_device(
    host: &str,
    user: &str,
    pass: &str,
    list: &str,
    baseline: &str,
) -> PollOutcome {
    poll_for_new_device_with_limit(host, user, pass, list, baseline, LIST_POLLS)
}

fn poll_for_new_device_with_limit(
    host: &str,
    user: &str,
    pass: &str,
    list: &str,
    baseline: &str,
    polls: u64,
) -> PollOutcome {
    for _ in 0..polls {
        std::thread::sleep(poll_interval());
        match ssh_list_devices_stdout(host, user, pass, list) {
            Ok(after) => {
                let added = xor_new_lines(baseline, &after);
                if !added.is_empty() {
                    return PollOutcome::Added(added);
                }
            }
            Err(reason) => return PollOutcome::Failed(reason),
        }
    }
    PollOutcome::NoChange
}

/// The flash device-list probe, per the operator's exact spec:
///
///   1. BEFORE triggering: capture `list_devices_cmd` output once;
///   2. trigger entry into flashing mode (loader_cmd over this DUT's
///      serial console, then recovery/maskrom relay as fallbacks);
///   3. AFTER triggering: capture `list_devices_cmd` again;
///   4. XOR the two outputs — the REMAINING (newly-appeared) lines are
///      this DUT's device: they land in the flash.devices row and the
///      test PASSES; an EMPTY XOR means the DUT never entered flashing
///      mode and the test FAILS.
///
/// The XOR rule is tool-agnostic — the `list_devices_cmd` command is run
/// verbatim, no tool name is ever hardcoded (Rockchip `upgrade_tool ld`,
/// Allwinner `sunxi-fel list`, fastboot devices, ... all adapt by just
/// configuring the command). Digit-run normalization keeps count-bearing
/// headers and re-enumeration number shifts from false-positiving, and
/// multi-DUT safety comes from the baseline cancellation: other boards
/// already sitting in flashing mode appear in BOTH captures and cancel
/// out — only this DUT's new device line remains.
///
/// After the trigger the list is POLLED for ~15 s (a Rockchip DUT takes
/// ~7 s to reboot into loader), and a baseline that is already non-empty
/// (the DUT — or another board — was already in flashing mode) produces
/// an explicit "already visible" failure detail instead of a bare
/// "no device" claim.
#[cfg(test)]
pub fn run_list_devices_probe(target: ProbeTarget) -> (ProbeMark, Option<String>) {
    run_list_devices_probe_profile(target, false)
}

/// TEST-button profile: keep the flash proof inside the shared 60s budget.
/// A loader command issued at U-Boot is retried in place instead of taking
/// the general 45s boot-to-shell fallback used by standalone diagnostics.
pub fn run_list_devices_probe_quick(target: ProbeTarget) -> (ProbeMark, Option<String>) {
    run_list_devices_probe_profile(target, true)
}

fn run_list_devices_probe_profile(target: ProbeTarget, quick: bool) -> (ProbeMark, Option<String>) {
    // The failure detail reports the ACTUAL elapsed time: the boot-resume
    // path stretches the probe well past a single poll window.
    let probe_started = std::time::Instant::now();
    let ProbeTarget::ListDevices {
        host,
        user,
        pass,
        tool,
        list_cmd,
        loader_cmd,
        maskrom_ch,
        recovery_ch,
        mcp_ports,
    } = target
    else {
        return (ProbeMark::Skip, None);
    };
    let list = format!("{} {}", tool.trim(), list_cmd.trim())
        .trim()
        .to_string();
    if list.is_empty() {
        return (ProbeMark::Skip, None);
    }
    let attempts = flash_attempts(&loader_cmd, recovery_ch, maskrom_ch);
    if attempts.is_empty() {
        return (ProbeMark::Skip, None);
    }
    // ① Before triggering — baseline capture. May legitimately be EMPTY
    // (nothing in flashing mode yet) or list OTHER boards already in
    // flashing mode — their lines cancel out of the XOR below. A
    // `sunxi-fel list` that exits non-zero with empty stderr is "zero FEL
    // devices", not a failure (see ssh_list_devices_stdout).
    let baseline = match ssh_list_devices_stdout(&host, &user, &pass, &list) {
        Ok(out) => out,
        Err(reason) => {
            return (
                ProbeMark::Err,
                Some(format!("list_devices_cmd over SSH failed: {reason}")),
            );
        }
    };
    // Why each attempt was skipped — folded into the final failure detail
    // so "XOR empty" explains itself instead of hiding the trigger failure.
    let mut skipped: Vec<String> = Vec::new();
    for attempt in attempts {
        match &attempt {
            FlashAttempt::SerialCmd(cmd) => {
                // CONTEXT-AWARE TRIGGER. `flash.loader_cmd` is a command
                // sent on the DUT's serial console, but the marker-echo
                // wrapper only works in a Linux shell, while U-Boot
                // commands (`db {loader}`, `rockusb 0 mmc 0`) only work
                // AT the U-Boot prompt. The U-Boot interrupt probe that
                // ran earlier in the same TEST leaves the DUT at the
                // prompt — sending the shell command there does NOTHING:
                // U-Boot echoes the marker-wrapped line (so
                // serial_send_command even "succeeds") and answers
                // "Unknown command". Resume the boot first and wait for
                // the shell.
                let state = mcp_dut_state(&mcp_ports);
                if state.as_deref() == Some("uboot") {
                    // ②a Try the loader_cmd RAW at the prompt first — a
                    // U-Boot-style command (`db {loader}`) switches the
                    // DUT into download mode within seconds; a
                    // shell-style command no-ops harmlessly. The FULL
                    // poll window applies (a download-mode entry slower
                    // than a few seconds must not false-fail), and only a
                    // NoChange after it concludes the DUT is still at the
                    // prompt — only then resume the boot.
                    // Sequence: the configured loader_cmd first (a
                    // U-Boot-style command like `db {loader}` switches the
                    // DUT into download mode within seconds; a shell-style
                    // command no-ops harmlessly), then `rbrom` — the
                    // Rockchip U-Boot standard "reset to maskrom" command —
                    // as a fallback so the flash path stays usable even
                    // when loader_cmd only works in Linux AND auto-login
                    // is broken.
                    let raw_sequence: [&str; 2] = [cmd, "rbrom"];
                    for raw_cmd in raw_sequence {
                        // A SUCCESSFUL trigger (`reboot loader`) restarts the
                        // DUT immediately — the command's echo marker never
                        // comes back, so serial_uboot_command reports a
                        // timeout. That is not a failed trigger: always poll
                        // for the device; the poll result is the truth.
                        let _ = super::mcp_probe_mark(
                            &mcp_ports,
                            "serial_uboot_command",
                            serde_json::json!({"command": raw_cmd, "timeout": 5}),
                        );
                        let outcome = if quick {
                            poll_for_new_device_with_limit(&host, &user, &pass, &list, &baseline, 8)
                        } else {
                            poll_for_new_device(&host, &user, &pass, &list, &baseline)
                        };
                        match outcome {
                            PollOutcome::Added(added) => {
                                return (ProbeMark::Ok, Some(added.join("\n")));
                            }
                            PollOutcome::Failed(reason) => {
                                return (
                                    ProbeMark::Err,
                                    Some(format!(
                                        "list_devices_cmd over SSH failed on re-check: {reason}"
                                    )),
                                );
                            }
                            PollOutcome::NoChange => {}
                        }
                    }
                    if quick {
                        continue;
                    }
                    // ②b Shell-style loader_cmd (`reboot loader`) needs
                    // Linux: `boot` resumes the interrupted boot, then
                    // wait for the shell (state `active`). The active
                    // gate needs login detection (target.login_user); the
                    // engine ALSO marks a responding console active via
                    // its heartbeat probe, so a shell without a login
                    // prompt still converges.
                    if super::mcp_probe_mark(
                        &mcp_ports,
                        "serial_uboot_command",
                        serde_json::json!({"command": "boot", "timeout": 5}),
                    ) != ProbeMark::Ok
                        || !wait_for_dut_state(&mcp_ports, "active", BOOT_TO_SHELL_WAIT_SECS)
                    {
                        skipped.push(format!(
                            "uboot resume: DUT never reached active shell within {BOOT_TO_SHELL_WAIT_SECS}s (login broken?)"
                        ));
                        continue;
                    }
                } else if matches!(state.as_deref(), Some("booting") | Some("connecting")) {
                    // Mid-boot (the baud / relay-verify probes reset the
                    // DUT earlier in the chain): wait for the shell so
                    // the loader command lands in a live console.
                    if !wait_for_dut_state(&mcp_ports, "active", BOOT_TO_SHELL_WAIT_SECS) {
                        skipped.push(format!(
                            "DUT was {state:?} and never reached active shell within {BOOT_TO_SHELL_WAIT_SECS}s (login broken?)"
                        ));
                        continue;
                    }
                }
                // ③ Shell trigger — serial_send_command addresses THIS
                // DUT's console (marker-wrapped, shell context). Same rule
                // as the U-Boot branch: a successful `reboot loader` kills
                // the shell before the marker returns, so poll regardless.
                let _ = super::mcp_probe_mark(
                    &mcp_ports,
                    "serial_send_command",
                    serde_json::json!({"command": cmd, "timeout": 5}),
                );
                // ③④ poll while the DUT reboots into loader. The marker
                // echo completes even when the command NO-OPs (a shell
                // command rejected at the U-Boot prompt), so a NoChange
                // falls THROUGH to the relay attempts below instead of
                // failing immediately — pressing recovery/maskrom after a
                // silent no-op is exactly the designed fallback.
                match poll_for_new_device(&host, &user, &pass, &list, &baseline) {
                    PollOutcome::Added(added) => return (ProbeMark::Ok, Some(added.join("\n"))),
                    PollOutcome::Failed(reason) => {
                        return (
                            ProbeMark::Err,
                            Some(format!(
                                "list_devices_cmd over SSH failed on re-check: {reason}"
                            )),
                        );
                    }
                    PollOutcome::NoChange => {
                        skipped.push(format!(
                            "shell trigger {cmd:?} sent (state {state:?}) but no new device appeared"
                        ));
                        continue;
                    }
                }
            }
            FlashAttempt::Relay(button) => {
                // ② Hold the recovery/maskrom button, pulse reset,
                // release — the DUT powers into MASKROM/loader mode.
                // serial_reset must NOT wait_boot or retry: the fallback
                // only pulses the relay, and the 15 s device poll below
                // is the verification — a boot-wait (up to 3 × ~45 s)
                // would blow the TEST spinner budget and drop the probe
                // mark on the deadline.
                let _ = super::mcp_probe_mark(
                    &mcp_ports,
                    "serial_button",
                    serde_json::json!({"button": button, "action": "press"}),
                );
                let _ = super::mcp_probe_mark(
                    &mcp_ports,
                    "serial_reset",
                    serde_json::json!({"wait_boot": false, "failure_retry": 1}),
                );
                // The release is the safety-critical step of the sandwich: a
                // stranded press boots the DUT into recovery/maskrom on the
                // next power-on. Retry it, and if it still fails, shout —
                // never discard the result silently.
                if !release_button(&mcp_ports, button) {
                    eprintln!(
                        "[flash] WARNING: serial_button {button} release FAILED after \
                         retries — the button may still be pressed. Release it before \
                         the next power-on (`dutabo button {button} release` or the \
                         serial_button tool)."
                    );
                }
                // Give the dev host a moment to enumerate the new USB
                // device, then ③ poll and ④ XOR.
                match poll_for_new_device(&host, &user, &pass, &list, &baseline) {
                    PollOutcome::Added(added) => return (ProbeMark::Ok, Some(added.join("\n"))),
                    PollOutcome::Failed(reason) => {
                        return (
                            ProbeMark::Err,
                            Some(format!(
                                "list_devices_cmd over SSH failed on re-check: {reason}"
                            )),
                        );
                    }
                    PollOutcome::NoChange => {
                        skipped.push(format!(
                            "relay {button} sandwich executed but no new device appeared"
                        ));
                        continue;
                    }
                }
            }
        }
    }
    // ④ XOR empty on every attempt → test FAILED. The detail reports the
    // ACTUAL elapsed time — the boot-resume path can stretch the probe
    // well past the single 15 s poll window.
    let elapsed = probe_started.elapsed().as_secs();
    let mut detail = no_device_detail(&baseline, &list, elapsed);
    if !skipped.is_empty() {
        detail.push_str(&format!(" Trigger attempts: {}.", skipped.join("; ")));
    }
    (ProbeMark::Err, Some(detail))
}

/// Failure detail when no NEW device line appeared after every attempt.
/// Two distinguishable cases:
///
///  - baseline EMPTY: nothing was in flashing mode before triggering and
///    nothing appeared afterwards — the DUT did not enter flashing mode.
///  - baseline NON-EMPTY: the list ALREADY showed device(s) before the
///    trigger and no new one appeared — either the DUT was already in
///    flashing mode (an unchanged visible device CAN be this DUT, in
///    which case the flashing path is usable), or the trigger failed
///    while another board sat in flashing mode. The detail lists what was
///    visible so the operator can tell.
fn no_device_detail(baseline: &str, list: &str, elapsed_secs: u64) -> String {
    let visible: Vec<&str> = baseline
        .lines()
        .map(|l| l.trim_end_matches('\r').trim())
        .filter(|l| !l.is_empty() && !is_list_boilerplate(l))
        .collect();
    if visible.is_empty() {
        format!(
            "XOR empty: no new device appeared in `{list}` after {elapsed_secs}s of trigger \
             attempts — the DUT did not enter flashing mode. Note the comparison normalizes \
             digit runs: two same-model devices whose lines differ only in numbers are \
             indistinguishable to this heuristic and read as \"already seen\"."
        )
    } else {
        format!(
            "device list unchanged after {elapsed_secs}s of trigger attempts: {} device(s) were \
             ALREADY visible — {} — if one of them is this DUT it is already in flashing mode \
             (flashing path usable), otherwise the trigger failed to add a new device",
            visible.len(),
            visible.join(" | "),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FlashAttempt {
    SerialCmd(String),
    Relay(&'static str),
}

fn flash_attempts(
    loader_cmd: &str,
    recovery_ch: Option<u8>,
    maskrom_ch: Option<u8>,
) -> Vec<FlashAttempt> {
    let mut attempts = Vec::new();
    if !loader_cmd.trim().is_empty() {
        attempts.push(FlashAttempt::SerialCmd(loader_cmd.trim().to_string()));
    }
    if recovery_ch.is_some() {
        attempts.push(FlashAttempt::Relay("recovery"));
    }
    if maskrom_ch.is_some() {
        attempts.push(FlashAttempt::Relay("maskrom"));
    }
    attempts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the fake-`setsid` tests below mutate the PROCESS-WIDE PATH env var
    /// (each test prepends its own fake dir). The harness may run tests on
    /// parallel threads regardless of `--test-threads`, so every test that
    /// touches PATH takes this lock — otherwise one test's fake `setsid`
    /// leaks into another test's spawn.
    static FAKE_SSH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    /// Uniquifies each probe test's temp dir — the pid-based dir is shared
    /// across tests in the process, and a parallel test's cleanup deleting
    /// it mid-setup is a real race.
    static PROBE_TEST_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    // ── End-to-end probe tests (fake MCP server + fake `setsid` ssh) ──

    /// When the fake MCP server makes the DUT's device line appear in the
    /// dev-host list.
    #[derive(Clone, Copy, PartialEq)]
    enum TriggerOn {
        /// `serial_send_command` (the shell-style trigger) is the trigger.
        SendCommand,
        /// The RAW `serial_uboot_command` loader command is the trigger.
        UbootRaw,
        /// No call ever triggers the device — the probe must fall through
        /// every attempt and fail with an empty XOR.
        Never,
    }

    /// Locate the `\r\n\r\n` header/body split in a raw HTTP request.
    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    /// Read one HTTP request body from the socket (headers + Content-Length).
    async fn read_http_body(sock: &mut tokio::net::TcpStream) -> Option<String> {
        use tokio::io::AsyncReadExt;
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 2048];
        loop {
            if let Some(pos) = find_header_end(&buf) {
                let header_text = String::from_utf8_lossy(&buf[..pos]);
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.trim()
                            .eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                if buf.len() >= pos + 4 + content_length {
                    let body = String::from_utf8_lossy(&buf[pos + 4..pos + 4 + content_length])
                        .to_string();
                    return Some(body);
                }
            }
            let n = sock.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
    }

    /// Write one HTTP response and close the connection.
    async fn write_http_response(
        sock: &mut tokio::net::TcpStream,
        status: &str,
        extra_headers: &str,
        body: &str,
    ) {
        use tokio::io::AsyncWriteExt;
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra_headers}\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.shutdown().await;
    }

    /// The JSON-RPC `tools/call` result frame carrying `payload` as the
    /// tool's text.
    fn tool_result_frame(payload: serde_json::Value) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [{"type": "text", "text": payload.to_string()}]
            }
        })
        .to_string()
    }

    /// Fake Streamable-HTTP MCP server: answers `initialize` (with a
    /// session header), `notifications/initialized`, and `tools/call` for
    /// serial_get_state / serial_uboot_command / serial_send_command.
    /// `state` starts at the given DUT state and flips to `active` when
    /// `serial_uboot_command boot` arrives; the trigger file appears when
    /// the configured trigger call arrives.
    struct FakeMcp {
        listener: tokio::net::TcpListener,
        state: std::sync::Arc<std::sync::Mutex<String>>,
        trigger_on: TriggerOn,
        /// serial_uboot_command `boot` answers `success: false` (the
        /// boot-resume path must fall through to the next attempt).
        boot_fails: bool,
        trigger_path: std::path::PathBuf,
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl FakeMcp {
        async fn bind(
            state: &str,
            trigger_on: TriggerOn,
            boot_fails: bool,
            trigger_path: std::path::PathBuf,
        ) -> FakeMcp {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            FakeMcp {
                listener,
                state: std::sync::Arc::new(std::sync::Mutex::new(state.to_string())),
                trigger_on,
                boot_fails,
                trigger_path,
                calls: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn port(&self) -> u16 {
            self.listener.local_addr().unwrap().port()
        }

        /// Serve until the runtime is shut down; returns the shared state
        /// and the captured call log ("tools/call:<tool>:<command>").
        fn spawn(
            self,
            rt: &tokio::runtime::Runtime,
        ) -> (
            std::sync::Arc<std::sync::Mutex<String>>,
            std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        ) {
            let state = self.state.clone();
            let calls = self.calls.clone();
            let state_ret = state.clone();
            let calls_ret = calls.clone();
            rt.spawn(async move {
                loop {
                    let Ok((mut sock, _)) = self.listener.accept().await else {
                        break;
                    };
                    let Some(body) = read_http_body(&mut sock).await else {
                        continue;
                    };
                    let Ok(req) = serde_json::from_str::<serde_json::Value>(&body) else {
                        continue;
                    };
                    let method = req["method"].as_str().unwrap_or("").to_string();
                    let tool = req["params"]["name"].as_str().unwrap_or("").to_string();
                    let command = req["params"]["arguments"]["command"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    let button = req["params"]["arguments"]["button"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    let detail = if tool == "serial_button" {
                        button
                    } else {
                        command.clone()
                    };
                    calls
                        .lock()
                        .unwrap()
                        .push(format!("{method}:{tool}:{detail}"));
                    match (method.as_str(), tool.as_str()) {
                        ("initialize", _) => {
                            write_http_response(
                                &mut sock,
                                "200 OK",
                                "Mcp-Session-Id: s1\r\n",
                                &serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "id": 0,
                                    "result": {
                                        "protocolVersion": "2024-11-05",
                                        "capabilities": {},
                                        "serverInfo": {"name": "fake", "version": "0.0.0"}
                                    }
                                })
                                .to_string(),
                            )
                            .await;
                        }
                        ("notifications/initialized", _) => {
                            write_http_response(&mut sock, "202 Accepted", "", "").await;
                        }
                        ("tools/call", "serial_get_state") => {
                            let payload = serde_json::json!({"state": *state.lock().unwrap()});
                            write_http_response(
                                &mut sock,
                                "200 OK",
                                "",
                                &tool_result_frame(payload),
                            )
                            .await;
                        }
                        ("tools/call", "serial_uboot_command") => {
                            if command == "boot" {
                                if self.boot_fails {
                                    write_http_response(
                                        &mut sock,
                                        "200 OK",
                                        "",
                                        &tool_result_frame(serde_json::json!({
                                            "success": false,
                                            "error": "boot refused (fake)"
                                        })),
                                    )
                                    .await;
                                    continue;
                                }
                                *state.lock().unwrap() = "active".to_string();
                            }
                            if self.trigger_on == TriggerOn::UbootRaw && command != "boot" {
                                std::fs::write(&self.trigger_path, "1").ok();
                            }
                            write_http_response(
                                &mut sock,
                                "200 OK",
                                "",
                                &tool_result_frame(serde_json::json!({
                                    "sent": command,
                                    "output": ""
                                })),
                            )
                            .await;
                        }
                        ("tools/call", "serial_button") => {
                            write_http_response(
                                &mut sock,
                                "200 OK",
                                "",
                                &tool_result_frame(serde_json::json!({"success": true})),
                            )
                            .await;
                        }
                        ("tools/call", "serial_reset") => {
                            write_http_response(
                                &mut sock,
                                "200 OK",
                                "",
                                &tool_result_frame(serde_json::json!({
                                    "success": true,
                                    "wait_boot": false
                                })),
                            )
                            .await;
                        }
                        ("tools/call", "serial_send_command") => {
                            if self.trigger_on == TriggerOn::SendCommand {
                                std::fs::write(&self.trigger_path, "1").ok();
                            }
                            write_http_response(
                                &mut sock,
                                "200 OK",
                                "",
                                &tool_result_frame(serde_json::json!({
                                    "output": "ok",
                                    "exit_code": 0,
                                    "timed_out": false
                                })),
                            )
                            .await;
                        }
                        _ => {
                            write_http_response(
                                &mut sock,
                                "200 OK",
                                "",
                                &tool_result_frame(serde_json::json!({})),
                            )
                            .await;
                        }
                    }
                }
            });
            (state_ret, calls_ret)
        }
    }

    /// Run the real probe end-to-end against a fake MCP server and a fake
    /// `setsid` ssh (whose "remote" `list_devices_cmd` reports the DUT once
    /// the trigger file exists). Returns the probe outcome, the captured
    /// MCP call log, and the temp dir (still on disk until the caller
    /// cleans up).
    fn run_probe_with_fakes(
        state: &str,
        loader_cmd: &str,
        trigger_on: TriggerOn,
        rt: &tokio::runtime::Runtime,
    ) -> ((ProbeMark, Option<String>), Vec<String>, std::path::PathBuf) {
        run_probe_with_fakes_full(state, loader_cmd, trigger_on, false, None, None, rt)
    }

    /// [`run_probe_with_fakes`] with the full knobs: `boot_fails` makes
    /// the boot-resume answer failure; `recovery_ch`/`maskrom_ch` arm the
    /// relay fallback attempts.
    #[allow(clippy::too_many_arguments)]
    fn run_probe_with_fakes_full(
        state: &str,
        loader_cmd: &str,
        trigger_on: TriggerOn,
        boot_fails: bool,
        recovery_ch: Option<u8>,
        maskrom_ch: Option<u8>,
        rt: &tokio::runtime::Runtime,
    ) -> ((ProbeMark, Option<String>), Vec<String>, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let _guard = FAKE_SSH_LOCK.lock().unwrap();
        // Serialize against dutabo.rs's own tests that assert on the
        // process-global MCP session cache contents (SESSION_TEST_MUTEX is
        // a tokio mutex — blocking_lock is fine from a sync test).
        let _session_guard = crate::tests::SESSION_TEST_MUTEX.blocking_lock();
        let seq = PROBE_TEST_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp =
            std::env::temp_dir().join(format!("dutabo-flash-probe-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let trigger_path = tmp.join("trigger");
        let fake_setsid = tmp.join("setsid");
        std::fs::write(
            &fake_setsid,
            "#!/bin/sh\n\
             if [ -f \"$TRIGGER_FILE\" ]; then\n\
             \x20 printf 'List of rockusb connected(1)\\nDevNo=0\\tVid=0x2207,Pid=0x350b,\
             LocationID=104\\tMode=Loader\\tSerialNo=TESTDUT\\n'\n\
             else\n\
             \x20 printf 'List of rockusb connected(0)\\n'\n\
             fi\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_setsid, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mcp = rt.block_on(FakeMcp::bind(
            state,
            trigger_on,
            boot_fails,
            trigger_path.clone(),
        ));
        let port = mcp.port();
        let (_, calls) = mcp.spawn(rt);
        // The session cache is process-global: drop any entry from an
        // earlier test so this probe starts a fresh handshake.
        *crate::MCP_SESSION.lock().unwrap() = None;

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(tmp.clone()).chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        // SAFETY: single-threaded tests (--test-threads=1) and FAKE_SSH_LOCK;
        // restored before any assertion can panic.
        unsafe { std::env::set_var("PATH", new_path) };
        unsafe { std::env::set_var("TRIGGER_FILE", &trigger_path) };
        unsafe { std::env::set_var("DUTABO_TEST_FAST_FLASH_POLL", "1") };
        let target = ProbeTarget::ListDevices {
            host: "192.0.2.1".into(),
            user: "linaro".into(),
            pass: String::new(),
            tool: "upgrade_tool".into(),
            list_cmd: "ld".into(),
            loader_cmd: loader_cmd.into(),
            maskrom_ch,
            recovery_ch,
            mcp_ports: vec![port],
        };
        let result = run_list_devices_probe(target);
        unsafe { std::env::set_var("PATH", old_path) };
        unsafe { std::env::remove_var("TRIGGER_FILE") };
        unsafe { std::env::remove_var("DUTABO_TEST_FAST_FLASH_POLL") };
        (result, calls.lock().unwrap().clone(), tmp)
    }

    /// THE reported regression: the U-Boot interrupt probe ran earlier in
    /// the same TEST and left the DUT AT the U-Boot prompt; the shell-style
    /// loader_cmd (`reboot loader`) is an "Unknown command" there. The
    /// probe must resume the boot (`boot`), wait for the Linux shell, THEN
    /// send the shell trigger.
    #[test]
    fn probe_recovers_from_uboot_prompt_before_shell_loader_cmd() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ((mark, detail), calls, tmp) =
            run_probe_with_fakes("uboot", "reboot loader", TriggerOn::SendCommand, &rt);
        let _ = std::fs::remove_dir_all(&tmp);
        rt.shutdown_timeout(std::time::Duration::from_secs(1));

        assert_eq!(
            mark,
            ProbeMark::Ok,
            "shell loader_cmd after a U-Boot prompt must still pass; detail={detail:?} calls={calls:?}"
        );
        let detail = detail.unwrap_or_default();
        assert!(detail.contains("TESTDUT"), "device line missing: {detail}");
        let raw_idx = calls
            .iter()
            .position(|c| c == "tools/call:serial_uboot_command:reboot loader");
        let boot_idx = calls
            .iter()
            .position(|c| c == "tools/call:serial_uboot_command:boot");
        let send_idx = calls
            .iter()
            .position(|c| c == "tools/call:serial_send_command:reboot loader");
        assert!(
            raw_idx.is_some(),
            "the loader_cmd must be tried RAW at the prompt first; calls={calls:?}"
        );
        assert!(
            boot_idx.is_some(),
            "must resume the boot from the U-Boot prompt; calls={calls:?}"
        );
        assert!(
            send_idx.is_some(),
            "must send the shell loader_cmd after the boot; calls={calls:?}"
        );
        assert!(
            raw_idx.unwrap() < boot_idx.unwrap() && boot_idx.unwrap() < send_idx.unwrap(),
            "order must be raw → boot → shell trigger; calls={calls:?}"
        );
    }

    /// The normal case: the DUT is already in Linux (state `active`) — the
    /// shell loader_cmd is sent directly, no U-Boot dance.
    #[test]
    fn probe_sends_shell_loader_cmd_directly_from_active_state() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ((mark, detail), calls, tmp) =
            run_probe_with_fakes("active", "reboot loader", TriggerOn::SendCommand, &rt);
        let _ = std::fs::remove_dir_all(&tmp);
        rt.shutdown_timeout(std::time::Duration::from_secs(1));

        assert_eq!(
            mark,
            ProbeMark::Ok,
            "shell loader_cmd from a live Linux shell must pass; detail={detail:?} calls={calls:?}"
        );
        assert!(
            detail
                .as_ref()
                .unwrap_or(&String::new())
                .contains("TESTDUT"),
            "device line missing: {detail:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("tools/call:serial_uboot_command")),
            "no U-Boot dance needed from active; calls={calls:?}"
        );
        assert!(
            calls.contains(&"tools/call:serial_send_command:reboot loader".to_string()),
            "shell trigger must be sent; calls={calls:?}"
        );
    }

    /// A U-Boot-style loader_cmd (`db loader.bin`) IS valid at the prompt:
    /// the probe tries it RAW first and passes without the boot-resume.
    #[test]
    fn probe_tries_uboot_style_loader_cmd_raw_at_the_prompt() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ((mark, detail), calls, tmp) =
            run_probe_with_fakes("uboot", "db loader.bin", TriggerOn::UbootRaw, &rt);
        let _ = std::fs::remove_dir_all(&tmp);
        rt.shutdown_timeout(std::time::Duration::from_secs(1));

        assert_eq!(
            mark,
            ProbeMark::Ok,
            "U-Boot-style loader_cmd at the prompt must pass; detail={detail:?} calls={calls:?}"
        );
        assert!(
            detail
                .as_ref()
                .unwrap_or(&String::new())
                .contains("TESTDUT"),
            "device line missing: {detail:?}"
        );
        assert!(
            calls.contains(&"tools/call:serial_uboot_command:db loader.bin".to_string()),
            "the loader_cmd must be tried RAW at the prompt; calls={calls:?}"
        );
        assert!(
            !calls.contains(&"tools/call:serial_uboot_command:boot".to_string()),
            "raw trigger succeeded — no boot-resume needed; calls={calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("tools/call:serial_send_command")),
            "a U-Boot command must never be shell-wrapped; calls={calls:?}"
        );
    }

    /// A trigger that never fires must fall THROUGH the shell attempt into
    /// the recovery/maskrom relay attempts (press + reset + release each)
    /// and only then fail — the marker echo completing is NOT proof the
    /// DUT entered flashing mode.
    #[test]
    fn probe_no_change_falls_through_to_relay_attempts() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ((mark, detail), calls, tmp) = run_probe_with_fakes_full(
            "active",
            "reboot loader",
            TriggerOn::Never,
            false,
            Some(1),
            Some(2),
            &rt,
        );
        let _ = std::fs::remove_dir_all(&tmp);
        rt.shutdown_timeout(std::time::Duration::from_secs(1));

        assert_eq!(
            mark,
            ProbeMark::Err,
            "a never-firing trigger must fail after ALL attempts; detail={detail:?} calls={calls:?}"
        );
        assert!(
            detail
                .as_ref()
                .unwrap_or(&String::new())
                .contains("XOR empty"),
            "failure detail must be the empty-XOR explanation: {detail:?}"
        );
        assert!(
            calls.contains(&"tools/call:serial_button:recovery".to_string())
                && calls.contains(&"tools/call:serial_button:maskrom".to_string()),
            "both relay fallbacks must be attempted; calls={calls:?}"
        );
        assert!(
            calls.contains(&"tools/call:serial_reset:".to_string()),
            "the relay fallback pulses reset; calls={calls:?}"
        );
    }

    /// A failed boot-resume (`serial_uboot_command boot` answers
    /// failure) must fall through to the next attempt instead of sending
    /// the shell trigger into a dead console.
    #[test]
    fn probe_boot_failure_falls_through() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ((mark, detail), calls, tmp) = run_probe_with_fakes_full(
            "uboot",
            "reboot loader",
            TriggerOn::SendCommand,
            true,
            None,
            None,
            &rt,
        );
        let _ = std::fs::remove_dir_all(&tmp);
        rt.shutdown_timeout(std::time::Duration::from_secs(1));

        assert_eq!(
            mark,
            ProbeMark::Err,
            "a failed boot-resume must end in a loud failure, not a shell trigger; \
             detail={detail:?} calls={calls:?}"
        );
        assert!(
            calls.contains(&"tools/call:serial_uboot_command:boot".to_string()),
            "boot must be attempted; calls={calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("tools/call:serial_send_command")),
            "no shell trigger after a failed boot-resume; calls={calls:?}"
        );
    }

    #[test]
    fn xor_new_lines_reports_only_additions() {
        let old = "device 0\nusb: x\n\n";
        let new = "device 0\nusb: x\nrkusb: y\n";
        assert_eq!(xor_new_lines(old, new), vec!["rkusb: y"]);
    }

    #[test]
    fn xor_new_lines_no_change_is_empty() {
        assert!(xor_new_lines("a\nb\n", "b\na\n").is_empty());
    }

    /// The lab-wall scenario that motivated the multiset diff: dozens of
    /// SAME-MODEL devices whose lines differ only in digit runs. Set
    /// membership collapses them all to one normalized key and a genuinely
    /// new device is "already seen" — the count must decide instead.
    #[test]
    fn xor_new_lines_detects_addition_among_identical_normalized_lines() {
        let device = |dev: usize, serial: &str| {
            format!(
                "DevNo={dev}\tVid=0x2207,Pid=0x350e,LocationID={}\tMode=Loader\tSerialNo={serial}",
                100 + dev
            )
        };
        let mut old_lines: Vec<String> = (1..=38)
            .map(|d| device(d, &format!("serial{d:04}")))
            .collect();
        old_lines.insert(0, "List of rockusb connected(38)".into());
        let mut new_lines = old_lines.clone();
        new_lines[0] = "List of rockusb connected(39)".into();
        let newcomer = device(39, "serial9999");
        new_lines.push(newcomer.clone());

        let added = xor_new_lines(&old_lines.join("\n"), &new_lines.join("\n"));
        assert_eq!(added, vec![newcomer], "exactly the new device is reported");
    }

    /// Re-enumeration that only SHIFTS numbers (same device, new DevNo /
    /// LocationID) must not be reported as an addition.
    #[test]
    fn xor_new_lines_ignores_pure_renumbering() {
        let old = "DevNo=1\tSerialNo=aaa\nDevNo=2\tSerialNo=bbb\n";
        let new = "DevNo=5\tSerialNo=aaa\nDevNo=6\tSerialNo=bbb\n";
        assert!(xor_new_lines(old, new).is_empty());
    }

    #[test]
    fn no_device_detail_is_an_explicit_failure() {
        let detail = no_device_detail("List of rockusb connected(0)\n", "upgrade_tool ld", 84);
        assert!(detail.contains("XOR empty"), "{detail}");
        assert!(detail.contains("did not enter flashing mode"), "{detail}");
        assert!(detail.contains("upgrade_tool ld"), "{detail}");
        assert!(detail.contains("84s"), "{detail}");
    }

    /// Real `upgrade_tool ld` layouts: another board already in Loader
    /// mode appears in BOTH captures and cancels; only this DUT's new
    /// device line remains.
    #[test]
    fn xor_upgrade_tool_ld_other_board_in_flashing_mode_cancels() {
        let old = "List of rockusb connected(1)\n\
                   DevNo=1\tVid=0x2207,Pid=0x3505,LocationID=101\tMode=Loader\tSerialNo=other-board\n";
        let new = "List of rockusb connected(2)\n\
                   DevNo=1\tVid=0x2207,Pid=0x3505,LocationID=101\tMode=Loader\tSerialNo=other-board\n\
                   DevNo=2\tVid=0x2207,Pid=0x350e,LocationID=113\tMode=Loader\tSerialNo=7208c83dd51a8ec8\n";
        assert_eq!(
            xor_new_lines(old, new),
            vec![
                "DevNo=2\tVid=0x2207,Pid=0x350e,LocationID=113\tMode=Loader\tSerialNo=7208c83dd51a8ec8"
            ]
        );
    }

    /// Real `sunxi-fel list` layouts (Allwinner): a foreign board already
    /// in FEL mode cancels; this DUT's new FEL line remains. An empty
    /// baseline (nothing in FEL mode) yields the whole new line.
    #[test]
    fn xor_sunxi_fel_list_format() {
        let old = "USB device 001:004   Allwinner A64    \n";
        let new = "USB device 001:004   Allwinner A64    \n\
                   USB device 001:007   Allwinner D1     f0d51ba:c11c0000:00000000:00000000\n";
        assert_eq!(
            xor_new_lines(old, new),
            vec!["USB device 001:007   Allwinner D1     f0d51ba:c11c0000:00000000:00000000"]
        );
        // Nothing in FEL mode before, one FEL device after → it is new.
        assert_eq!(
            xor_new_lines("", "USB device 001:007   Allwinner D1    \n"),
            vec!["USB device 001:007   Allwinner D1"]
        );
    }

    /// Baseline already non-empty (the DUT — or another board — was
    /// already in flashing mode): the detail must SAY so and list the
    /// visible devices instead of claiming nothing appeared.
    #[test]
    fn no_device_detail_reports_already_visible_devices() {
        let detail = no_device_detail(
            "List of rockusb connected(1)\n\
             DevNo=1\tVid=0x2207,Pid=0x350e,LocationID=113\tMode=Loader\tSerialNo=7208c83dd51a8ec8\n",
            "upgrade_tool ld",
            96,
        );
        assert!(detail.contains("ALREADY visible"), "{detail}");
        assert!(detail.contains("7208c83dd51a8ec8"), "{detail}");
        assert!(detail.contains("1 device(s)"), "{detail}");
    }

    #[test]
    fn attempts_are_loader_then_recovery_then_maskrom() {
        assert_eq!(
            flash_attempts("db loader.bin", Some(2), Some(3)),
            [
                FlashAttempt::SerialCmd("db loader.bin".into()),
                FlashAttempt::Relay("recovery"),
                FlashAttempt::Relay("maskrom"),
            ]
        );
        assert_eq!(
            flash_attempts("", Some(2), Some(3)),
            [
                FlashAttempt::Relay("recovery"),
                FlashAttempt::Relay("maskrom"),
            ]
        );
        assert!(flash_attempts("", None, None).is_empty());
    }

    /// The classic re-image / fresh-workstation failure: ssh key auth
    /// denied, no sshpass. The probe must surface an ACTIONABLE hint
    /// (ssh-copy-id), not a bare "over SSH failed".
    #[test]
    fn ssh_auth_failure_hint_mentions_ssh_copy_id() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = FAKE_SSH_LOCK.lock().unwrap();

        let tmp = std::env::temp_dir().join(format!("dutabo-fake-setsid-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let fake = tmp.join("setsid");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             echo 'linaro@192.0.2.1: Permission denied (publickey,password).' >&2\n\
             exit 255\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(tmp.clone()).chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        // SAFETY: this crate's tests run single-threaded (--test-threads=1);
        // PATH is restored before any assertion can panic.
        unsafe { std::env::set_var("PATH", new_path) };
        let result = ssh_list_devices_stdout("192.0.2.1", "linaro", "", "upgrade_tool ld");
        unsafe { std::env::set_var("PATH", old_path) };
        let _ = std::fs::remove_dir_all(&tmp);

        match result {
            Err(msg) => {
                assert!(
                    msg.contains("ssh-copy-id linaro@192.0.2.1"),
                    "hint missing from: {msg}"
                );
                assert!(
                    msg.contains("Permission denied"),
                    "reason missing from: {msg}"
                );
            }
            Ok(_) => panic!("fake setsid should have failed key auth"),
        }
    }

    /// When a password IS configured but sshpass is absent, the hint must
    /// still point at the stable fix (key auth), not dead-end at sshpass.
    #[test]
    fn ssh_auth_failure_with_pass_but_no_sshpass_still_hints_copy_id() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = FAKE_SSH_LOCK.lock().unwrap();

        let tmp = std::env::temp_dir().join(format!("dutabo-fake-ssh-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let fake = tmp.join("setsid");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             echo 'linaro@192.0.2.1: Permission denied (publickey).' >&2\n\
             exit 255\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(tmp.clone()).chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        // SAFETY: single-threaded tests; restored before assertions.
        unsafe { std::env::set_var("PATH", new_path) };
        let result = ssh_list_devices_stdout("192.0.2.1", "linaro", "secret", "upgrade_tool ld");
        unsafe { std::env::set_var("PATH", old_path) };
        let _ = std::fs::remove_dir_all(&tmp);

        match result {
            Err(msg) => {
                assert!(msg.contains("ssh-copy-id"), "hint missing from: {msg}");
                assert!(msg.contains("sshpass"), "sshpass note missing from: {msg}");
            }
            Ok(_) => panic!("fake setsid should have failed key auth"),
        }
    }

    /// `sunxi-fel list` with zero FEL devices exits 1 and prints NOTHING
    /// (no stderr) — the probe must read that as an empty device list,
    /// not an ssh/tool failure.
    #[test]
    fn list_command_nonzero_exit_with_empty_stderr_is_no_devices() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = FAKE_SSH_LOCK.lock().unwrap();

        let tmp = std::env::temp_dir().join(format!("dutabo-fake-sunxi-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let fake = tmp.join("setsid");
        std::fs::write(&fake, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(tmp.clone()).chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        // SAFETY: single-threaded tests; restored before assertions.
        unsafe { std::env::set_var("PATH", new_path) };
        let result = ssh_list_devices_stdout("192.0.2.1", "linaro", "", "sunxi-fel list");
        unsafe { std::env::set_var("PATH", old_path) };
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(
            result,
            Ok(String::new()),
            "empty stderr + non-zero exit = no devices in flashing mode"
        );
    }

    #[test]
    fn list_command_nonzero_exit_with_stdout_surfaces_command_error() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = FAKE_SSH_LOCK.lock().unwrap();
        let tmp =
            std::env::temp_dir().join(format!("dutabo-fake-invalid-list-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let fake = tmp.join("setsid");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'command is invalid, check usage'\nexit 255\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(tmp.clone()).chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        unsafe { std::env::set_var("PATH", new_path) };
        let result = preflight_list_devices("192.0.2.1", "linaro", "", "", "upgrade_tool l");
        unsafe { std::env::set_var("PATH", old_path) };
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(result.0, ProbeMark::Err);
        assert!(
            result
                .1
                .as_deref()
                .is_some_and(|detail| detail.contains("command is invalid"))
        );
    }

    /// A flash tool that fails on stderr (upgrade_tool's
    /// usb_claim_interface / permission errors) must still surface as an
    /// error, never as "no devices".
    #[test]
    fn list_command_real_error_on_stderr_surfaces() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = FAKE_SSH_LOCK.lock().unwrap();

        let tmp = std::env::temp_dir().join(format!("dutabo-fake-tool-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let fake = tmp.join("setsid");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             echo 'usb_claim_interface failed for 2207:350e: Operation not permitted' >&2\n\
             exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(tmp.clone()).chain(std::env::split_paths(&old_path)),
        )
        .unwrap();
        // SAFETY: single-threaded tests; restored before assertions.
        unsafe { std::env::set_var("PATH", new_path) };
        let result = ssh_list_devices_stdout("192.0.2.1", "linaro", "", "upgrade_tool ld");
        unsafe { std::env::set_var("PATH", old_path) };
        let _ = std::fs::remove_dir_all(&tmp);

        match result {
            Err(msg) => {
                assert!(msg.contains("usb_claim_interface failed"), "got: {msg}");
            }
            Ok(_) => panic!("stderr error must surface, not read as no devices"),
        }
    }

    // ── wait_with_timeout: real-subprocess cap semantics ─────────────────

    /// A child that finishes well inside the cap returns its exit status.
    #[test]
    fn wait_with_timeout_returns_status_for_prompt_child() {
        use std::process::Command;
        let mut child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let status = wait_with_timeout(&mut child, 10).expect("finishes in time");
        assert!(status.success());
    }

    /// A child that outlives the cap is killed and reaped: `None` comes
    /// back and the call returns promptly instead of parking the probe
    /// thread forever (the hung-remote-command leak).
    #[test]
    fn wait_with_timeout_kills_a_hung_child() {
        use std::process::Command;
        let mut child = Command::new("sh").args(["-c", "sleep 30"]).spawn().unwrap();
        let started = std::time::Instant::now();
        let status = wait_with_timeout(&mut child, 1);
        let elapsed = started.elapsed();
        assert!(status.is_none(), "capped child reports None");
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "cap must fire near 1s, took {elapsed:?}"
        );
        // The child was reaped by the kill path — a second wait must not
        // see a zombie.
        assert!(child.wait().is_ok());
    }
}
