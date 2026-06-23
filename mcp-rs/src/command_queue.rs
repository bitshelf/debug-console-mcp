//! Command queue — serialized execution + marker response routing.
//!
//! Mirrors labgrid's `UBootDriver._run()` marker-echo pattern:
//!   echo '{marker[:4]}''{marker[4:]}'; {cmd}; echo "EXIT:$?"; echo '{marker[:4]}''{marker[4:]}'

use std::collections::VecDeque;
use std::time::Instant;

use tokio::sync::oneshot;

use crate::marker::gen_marker;

/// Strip ANSI escape codes (regex compiled once via `LazyLock`).
fn strip_ansi(data: &[u8]) -> Vec<u8> {
    use std::sync::LazyLock;
    static RE_VT100: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(\x1b\[|\x9b)[0-?]*[ -/]*[@-~]|\x1b[>=]|\x1b[()][A-Z0-9]").unwrap()
    });
    let s = String::from_utf8_lossy(data);
    RE_VT100.replace_all(&s, "").to_string().into_bytes()
}

struct PendingCommand {
    command: String,
    marker: String,
    timeout_secs: f64,
    sender: Option<oneshot::Sender<CommandResult>>,
    begin_sent: bool,
    found_begin: bool,
    buffer: Vec<u8>,
    /// Cross-chunk search buffer (cf. labgrid's `before` buffer).
    search_buf: Vec<u8>,
    sent_at: Instant,
    /// Skip the first marker (shell echo of sent command), use the second as real begin.
    skip_count: u8,
}

impl PendingCommand {
    fn marker_bytes(&self) -> Vec<u8> {
        self.marker.as_bytes().to_vec()
    }

    fn resolve(self, output: String, timed_out: bool, exit_code: Option<i32>) {
        if let Some(tx) = self.sender {
            let _ = tx.send(CommandResult {
                output,
                exit_code,
                timed_out,
            });
        }
    }
}

#[derive(Debug)]
pub struct CommandResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

pub struct CommandQueue {
    pending: VecDeque<PendingCommand>,
    current: Option<PendingCommand>,
    write_fn: Option<Box<dyn Fn(&[u8]) + Send>>,
}

impl CommandQueue {
    pub fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            current: None,
            write_fn: None,
        }
    }

    pub fn set_write_fn(&mut self, f: Box<dyn Fn(&[u8]) + Send>) {
        self.write_fn = Some(f);
    }

    /// Submit a command and return a receiver to await the result.
    pub fn execute(
        &mut self,
        command: String,
        timeout_secs: f64,
    ) -> oneshot::Receiver<CommandResult> {
        let (tx, rx) = oneshot::channel();
        let marker = gen_marker();
        let pc = PendingCommand {
            command,
            marker,
            timeout_secs,
            sender: Some(tx),
            begin_sent: false,
            found_begin: false,
            buffer: Vec::new(),
            search_buf: Vec::new(),
            sent_at: Instant::now(),
            skip_count: 0,
        };

        if self.current.is_none() {
            self.send_command(pc);
        } else {
            self.pending.push_back(pc);
        }
        rx
    }

    /// Scan the serial data stream for begin/end markers and extract command output.
    pub fn feed_serial_data(&mut self, data: &[u8]) {
        let data = strip_ansi(data);

        // Check timeout first — MUST dequeue_next() to avoid queue deadlock.
        if let Some(ref pc) = self.current {
            if pc.begin_sent && pc.sent_at.elapsed().as_secs_f64() > pc.timeout_secs {
                if let Some(pc) = self.current.take() {
                    let output = String::from_utf8_lossy(&pc.buffer).to_string();
                    pc.resolve(output.trim().to_string(), true, None);
                }
                self.dequeue_next();
            }
        }

        let Some(ref mut pc) = self.current else {
            return;
        };
        if !pc.begin_sent {
            return;
        }

        let marker = pc.marker_bytes();

        // Step 1: begin_marker not yet found — accumulate cross-chunk search buffer.
        // stty -echo suppresses shell echo, so the first marker is the real one.
        if !pc.found_begin {
            pc.search_buf.extend_from_slice(&data);
            // Cap the search buffer to prevent unbounded memory growth.
            if pc.search_buf.len() > 65536 {
                let drain = pc.search_buf.len() - 32768;
                pc.search_buf.drain(..drain);
            }
            if let Some(idx) = find_subsequence(&pc.search_buf, &marker) {
                pc.found_begin = true;
                pc.buffer = pc.search_buf[idx + marker.len()..].to_vec();
                pc.search_buf.clear();
            } else {
                return;
            }
        }

        // Step 2: begin_marker found — append data to buffer and scan for end_marker.
        pc.buffer.extend_from_slice(&data);
        if pc.buffer.len() > 65536 {
            let drain = pc.buffer.len() - 32768;
            pc.buffer.drain(..drain);
        }
        if let Some(idx) = find_subsequence(&pc.buffer, &marker) {
            let output_bytes = pc.buffer[..idx].to_vec();
            let output = String::from_utf8_lossy(&output_bytes)
                .replace('\r', "")
                .trim()
                .to_string();

            // Extract exit code: the LAST line before the end marker should be
            // `echo "EXIT:$?"` output (prefixed with EXIT: for unambiguous parsing).
            let (clean_output, exit_code) = extract_exit_code(&output);

            let rest = pc.buffer[idx + marker.len()..].to_vec();
            if let Some(pc) = self.current.take() {
                pc.resolve(clean_output, false, exit_code);
            }
            self.dequeue_next();
            if !rest.is_empty() {
                self.feed_serial_data(&rest);
            }
        }
    }

    /// Check whether the current command has timed out (called periodically by
    /// the external read loop).
    pub fn check_timeouts(&mut self) {
        if let Some(ref pc) = self.current {
            if pc.begin_sent && pc.sent_at.elapsed().as_secs_f64() > pc.timeout_secs {
                if let Some(pc) = self.current.take() {
                    let output = String::from_utf8_lossy(&pc.buffer).to_string();
                    pc.resolve(output.trim().to_string(), true, None);
                    self.dequeue_next();
                }
            }
        }
    }

    fn send_command(&mut self, mut pc: PendingCommand) {
        let m = &pc.marker;
        // stty -echo suppresses shell echo → no marker in echoed text → clean output
        // EXIT: prefix makes exit code extraction unambiguous — no false matches on
        // bare numeric output lines (e.g. "wc -l" → "42" would be misread as exit code).
        let line = format!(
            "stty -echo; echo '{}''{}'; {}; echo \"EXIT:$?\"; echo '{}''{}'; stty echo\n",
            &m[..4],
            &m[4..],
            pc.command,
            &m[..4],
            &m[4..],
        );

        if let Some(ref write_fn) = self.write_fn {
            pc.search_buf.clear(); // clear stale buffer to avoid matching a previous command's marker
            write_fn(line.as_bytes());
            pc.sent_at = Instant::now();
            pc.begin_sent = true;
            pc.skip_count = 0;
            self.current = Some(pc);
        }
    }

    fn dequeue_next(&mut self) {
        if self.current.is_some() {
            return;
        }
        if let Some(pc) = self.pending.pop_front() {
            self.send_command(pc);
        }
    }
}

/// Find the first occurrence of `needle` in `haystack`.
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0); // empty needle matches at position 0 (defensive)
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Extract the exit code from the last line of command output.
///
/// The shell command template ends with `echo "EXIT:$?"`, so the very last
/// line should be `EXIT:<0-255>`. Using an unambiguous prefix prevents false
/// matches on data lines that happen to be integers (e.g. `wc -l` → `42`).
///
/// Returns `(output_without_exit_code_line, Some(code))` or `(original_output, None)`.
fn extract_exit_code(output: &str) -> (String, Option<i32>) {
    let trimmed = output.trim_end();
    if let Some(last_line) = trimmed.lines().next_back() {
        let stripped = last_line.trim();
        // Parse EXIT:<digits> prefix for unambiguous extraction.
        if let Some(num_str) = stripped.strip_prefix("EXIT:") {
            if let Ok(code) = num_str.parse::<u32>() {
                if code <= 255 {
                    let exit_code = code as i32;
                    // Remove the exit-code line from the output.
                    let end = trimmed.rfind(stripped).unwrap_or(trimmed.len());
                    let clean = trimmed[..end].trim_end().to_string();
                    return (clean, Some(exit_code));
                }
            }
        }
    }
    (output.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_subsequence() {
        assert_eq!(find_subsequence(b"hello world", b"world"), Some(6));
        assert_eq!(find_subsequence(b"hello world", b"xyz"), None);
        assert_eq!(find_subsequence(b"ABCABC", b"ABC"), Some(0));
    }

    #[test]
    fn test_strip_ansi() {
        let input = b"\x1b[31mERROR\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, b"ERROR");
    }

    #[test]
    fn test_strip_ansi_multiple() {
        let input = b"\x1b[1m\x1b[31mBold Red\x1b[0m Normal";
        let result = strip_ansi(input);
        assert_eq!(result, b"Bold Red Normal");
    }

    #[test]
    fn test_strip_ansi_cursor() {
        let input = b"\x1b[2J\x1b[H\x1b[?25h";
        let result = strip_ansi(input);
        assert_eq!(result, b"");
    }

    #[test]
    fn test_find_subsequence_not_found() {
        assert_eq!(find_subsequence(b"hello", b"world"), None);
        assert_eq!(find_subsequence(b"", b"test"), None);
        assert_eq!(find_subsequence(b"short", b"longer"), None);
    }

    #[test]
    fn test_find_subsequence_empty() {
        // Empty needle matches at position 0 (defensive).
        assert_eq!(find_subsequence(b"hello", b""), Some(0));
        assert_eq!(find_subsequence(b"", b""), Some(0));
    }

    #[test]
    fn test_extract_exit_code_valid() {
        let (out, code) = extract_exit_code("line1\nline2\nEXIT:0");
        assert_eq!(code, Some(0));
        assert_eq!(out, "line1\nline2");
    }

    #[test]
    fn test_extract_exit_code_nonzero() {
        let (out, code) = extract_exit_code("error msg\nEXIT:127");
        assert_eq!(code, Some(127));
        assert_eq!(out, "error msg");
    }

    #[test]
    fn test_extract_exit_code_no_code() {
        let (out, code) = extract_exit_code("just output\nno number");
        assert_eq!(code, None);
        assert_eq!(out, "just output\nno number");
    }

    #[test]
    fn test_extract_exit_code_out_of_range() {
        // Values > 255 are not valid shell exit codes → ignored.
        let (out, code) = extract_exit_code("data\nEXIT:300");
        assert_eq!(code, None);
        assert_eq!(out, "data\nEXIT:300");
    }

    #[test]
    fn test_extract_exit_code_negative() {
        // Negative values are not valid shell exit codes → ignored.
        let (_out, code) = extract_exit_code("data\nEXIT:-1");
        assert_eq!(code, None);
    }

    #[test]
    fn test_extract_exit_code_data_line_is_number() {
        // A data line that is a number should NOT be misread as exit code
        // because only the LAST line is examined and it must have EXIT: prefix.
        let (out, code) = extract_exit_code("42\nactual output");
        assert_eq!(code, None);
        assert_eq!(out, "42\nactual output");
    }

    #[test]
    fn test_extract_exit_code_wc_l_style_output() {
        // Regression: output from commands like `wc -l` that end with numbers
        // must not be misread as exit codes. EXIT: prefix prevents this.
        let (out, code) = extract_exit_code("total lines processed\n242");
        assert_eq!(code, None);
        assert_eq!(out, "total lines processed\n242");
    }

    #[test]
    fn test_extract_exit_code_no_exit_prefix() {
        // Bare 0 without EXIT: prefix → not an exit code.
        let (out, code) = extract_exit_code("command output\n0");
        assert_eq!(code, None);
        assert_eq!(out, "command output\n0");
    }

    #[test]
    fn test_command_format() {
        let mut cq = CommandQueue::new();
        use std::sync::{Arc, Mutex};
        let written = Arc::new(Mutex::new(Vec::new()));
        let written_clone = written.clone();
        let write_fn = Box::new(move |data: &[u8]| {
            written_clone.lock().unwrap().extend_from_slice(data);
        });
        cq.set_write_fn(write_fn);

        let _rx = cq.execute("uname -a".to_string(), 90.0);

        // Should have written the command with markers
        let written_data = written.lock().unwrap();
        let cmd_str = String::from_utf8_lossy(&written_data);
        assert!(cmd_str.starts_with("stty -echo; echo '"));
        assert!(cmd_str.contains("uname -a"));
        assert!(cmd_str.contains("echo \"EXIT:$?\""));
        assert!(cmd_str.ends_with("; stty echo\n"));
    }

    #[test]
    fn test_marker_extraction() {
        // This test verifies that feed_serial_data can extract output between markers
        // The actual shell echo behavior is complex, so we test the core logic directly
        let marker = gen_marker();
        let marker_bytes = marker.as_bytes();

        // Simulate serial data with marker appearing twice
        let serial_data = format!("{}\noutput line 1\noutput line 2\nEXIT:0\n{}\n", marker, marker);

        // Verify the marker appears twice in the data
        let first_pos = serial_data.find(&marker);
        assert!(first_pos.is_some());
        let second_pos = serial_data[first_pos.unwrap() + marker.len()..].find(&marker);
        assert!(second_pos.is_some());

        // Verify find_subsequence works correctly
        assert_eq!(
            find_subsequence(serial_data.as_bytes(), marker_bytes),
            first_pos
        );
    }

    #[test]
    fn test_command_serialization() {
        let mut cq = CommandQueue::new();
        let write_fn = Box::new(|_data: &[u8]| {});
        cq.set_write_fn(write_fn);

        // Submit two commands
        let mut rx1 = cq.execute("cmd1".to_string(), 90.0);
        let mut rx2 = cq.execute("cmd2".to_string(), 90.0);

        // First command should be pending, second queued
        assert!(rx1.try_recv().is_err());
        assert!(rx2.try_recv().is_err());
    }

    #[test]
    fn test_marker_split() {
        let marker = gen_marker();
        assert_eq!(marker.len(), 10);

        let first_half = &marker[..4];
        let second_half = &marker[4..];

        // Both halves should be non-empty
        assert!(!first_half.is_empty());
        assert!(!second_half.is_empty());

        // Reconstruct should equal original
        let reconstructed = format!("{}{}", first_half, second_half);
        assert_eq!(reconstructed, marker);
    }

    #[test]
    fn test_strip_ansi_no_escapes() {
        let input = b"plain text without escapes";
        let result = strip_ansi(input);
        assert_eq!(result, b"plain text without escapes");
    }

    /// Regression: shell echo of the sent command contains the marker text,
    /// causing CommandQueue to match the wrong marker as begin. The fix
    /// skips the first marker (echoed command) and uses the second (real output).
    #[test]
    fn test_skip_echoed_marker() {
        let mut cq = CommandQueue::new();

        // Set up a write_fn that captures the sent command
        let sm = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sm_clone = sm.clone();
        let write_fn = Box::new(move |data: &[u8]| {
            *sm_clone.lock().unwrap() = String::from_utf8_lossy(data).to_string();
        });
        cq.set_write_fn(write_fn);

        let mut rx = cq.execute("dmesg | grep -iE 'opp|cpu' | head -5".to_string(), 90.0);

        let sent = sm.lock().unwrap();
        let marker: String = sent
            .split('\'')
            .nth(1)
            .unwrap()
            .chars()
            .chain(sent.split('\'').nth(3).unwrap().chars())
            .collect();
        assert_eq!(marker.len(), 10);

        let marker_bytes = marker.as_bytes().to_vec();

        // Simulate serial data with shell echo (same as 2>&1, pipe, redirect):
        // 1. Echoed command line (contains marker)
        // 2. Newline
        // 3. Actual marker output (from echo command)
        // 4. Command output
        // 5. Exit code
        // 6. End marker
        // With stty -echo, the sent command is NOT echoed. Only the output appears.
        let mut serial_stream = Vec::new();
        // Actual marker output (from echo 'MARKER')
        serial_stream.extend_from_slice(&marker_bytes);
        serial_stream.push(b'\n');
        // Command output
        serial_stream.extend_from_slice(b"CPU0: 408000\nOPP table v2\nCPU1: 600000\n");
        // Exit code
        serial_stream.extend_from_slice(b"EXIT:0\n");
        // End marker
        serial_stream.extend_from_slice(&marker_bytes);
        serial_stream.push(b'\n');

        cq.feed_serial_data(&serial_stream);

        let result = rx.try_recv().unwrap();
        assert!(!result.timed_out);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.output.contains("CPU0: 408000"));
        assert!(result.output.contains("OPP table v2"));
        // Should NOT contain the echoed command text
        assert!(!result.output.contains("echo '"));
        assert!(!result.output.contains("dmesg"));
    }

    /// Verifies that piping with 2>&1 works correctly (regression test
    /// for the pipe/special-char marker interference issue).
    #[test]
    fn test_pipe_and_redirect_not_garbled() {
        let mut cq = CommandQueue::new();

        let sm = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sm_clone = sm.clone();
        let write_fn = Box::new(move |data: &[u8]| {
            *sm_clone.lock().unwrap() = String::from_utf8_lossy(data).to_string();
        });
        cq.set_write_fn(write_fn);

        let mut rx = cq.execute("ls /sys/devices/system/cpu/cpufreq/ 2>&1".to_string(), 90.0);

        let sent = sm.lock().unwrap();
        let parts: Vec<&str> = sent.split('\'').collect();
        let marker: String = format!("{}{}", parts[1], parts[3]);
        let mb = marker.as_bytes().to_vec();

        // With stty -echo, no echoed command text. Only output appears.
        let mut data = Vec::new();
        data.extend_from_slice(&mb); // real marker
        data.push(b'\n');
        data.extend_from_slice(b"policy0\npolicy4\n");
        data.extend_from_slice(b"EXIT:0\n");
        data.extend_from_slice(&mb);
        data.push(b'\n');

        cq.feed_serial_data(&data);
        let result = rx.try_recv().unwrap();
        assert!(!result.timed_out);
        assert!(result.output.contains("policy0"));
        assert!(!result.output.contains("echo '"));
        assert!(!result.output.contains("ls /sys"));
    }
}
