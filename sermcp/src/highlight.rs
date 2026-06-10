//! Shell prompt syntax highlighting for serial console output.
//!
//! Detects Linux shell prompts (bash, ash, zsh) in serial data and applies
//! ANSI SGR color codes for readability:
//!   - `user@host` → green (bold)
//!   - `/path`       → blue (bold)
//!   - `$` / `#`     → white (bold)
//!
//! Supports two common prompt formats:
//!   - `user@host:/path$ `       (colon-separated)
//!   - `[user@host /path]# `     (bracketed)

use std::ops::Range;
use std::sync::LazyLock;

// ── ANSI codes ──────────────────────────────────────────────────────────────

const GREEN_BOLD: &[u8] = b"\x1b[1;32m";
const BLUE_BOLD: &[u8] = b"\x1b[1;34m";
const WHITE_BOLD: &[u8] = b"\x1b[1;37m";
const RESET: &[u8] = b"\x1b[0m";

// ── Public API ──────────────────────────────────────────────────────────────

/// Apply shell prompt syntax highlighting to serial output data.
///
/// Each line is examined; if it matches a known prompt pattern, ANSI colors are
/// injected. Non-prompt lines pass through unchanged.
pub fn highlight_serial_prompt(data: &[u8], out: &mut Vec<u8>) {
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

/// Stateful chunk-aware prompt highlighter for STREAMING relays.
///
/// `highlight_serial_prompt` treats every chunk as a complete document: a
/// chunk that ends MID-LINE (a prompt still arriving, e.g. right after an
/// `ls` burst) matches nothing, so the prompt renders unhighlighted — the
/// tail chunk can never repair it. This wrapper carries the trailing
/// partial line (no `\n`) into the next chunk, so a prompt split across
/// any chunk boundary is still highlighted as one line.
///
/// Latency contract (typed echo must NOT wait for
/// Enter): partial carries are held at most [`Self::DEBOUNCE`] — the
/// caller runs [`Self::flush_quiet`] on a timer; bytes that arrive within
/// the window merge (split prompts), the quiet window flushes the echo.
/// A carry that already fully matches a prompt is emitted immediately
/// (idle prompts never see a newline).
pub struct PromptHighlighter {
    carry: Vec<u8>,
    last_activity: Option<std::time::Instant>,
}

impl PromptHighlighter {
    /// How long a partial line may be held while more bytes might merge.
    /// Prompt fragments arrive within one TCP burst (microseconds apart);
    /// typed echo trickles per keystroke — 40ms is imperceptible for echo
    /// yet safely covers read fragmentation.
    pub const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(40);

    pub fn new() -> Self {
        Self {
            carry: Vec::new(),
            last_activity: None,
        }
    }

    /// Highlight one incoming chunk; partial trailing lines are buffered.
    /// `now` is the caller's clock (tests inject deterministic instants).
    pub fn highlight(&mut self, data: &[u8], now: std::time::Instant, out: &mut Vec<u8>) {
        self.last_activity = Some(now);
        let mut line = Vec::with_capacity(self.carry.len() + data.len());
        line.extend_from_slice(&self.carry);
        self.carry.clear();
        line.extend_from_slice(data);

        // Everything up to the LAST '\n' is complete — highlight it; the
        // remainder (possibly mid-prompt) waits for the next chunk.
        match line.iter().rposition(|&b| b == b'\n') {
            Some(last_nl) => {
                highlight_serial_prompt(&line[..=last_nl], out);
                self.carry.extend_from_slice(&line[last_nl + 1..]);
            }
            None => self.carry = line,
        }
        // An IDLE prompt is the stream's last line and never sees a
        // newline — pure buffering would leave it white until the next
        // command. Emit it AS SOON AS it already matches the complete
        // prompt pattern (the trailing "$ "/"# " is part of the match);
        // a later command echo appends as its own plain segment, visually
        // identical. Still-incomplete carries keep waiting (see
        // (see [`Self::flush_quiet`]).
        self.emit_matched_carry(out);
    }

    /// Debounce tick: when no bytes arrived for [`Self::DEBOUNCE`], the
    /// partial line is as complete as it will get without new input —
    /// flush it (prompt-match attempt, else raw) so typed ECHO is visible
    /// immediately instead of waiting for Enter.
    pub fn flush_quiet(&mut self, now: std::time::Instant, out: &mut Vec<u8>) {
        let quiet = match self.last_activity {
            Some(t) => now.duration_since(t) >= Self::DEBOUNCE,
            None => false,
        };
        if quiet && !self.carry.is_empty() {
            highlight_serial_prompt_line(&self.carry, out);
            self.carry.clear();
            self.last_activity = Some(now);
        }
    }

    /// Flush the buffered partial line unhighlighted (stream end / before
    /// losing the tail). Call when the relay loop exits.
    pub fn finish(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.carry);
        self.carry.clear();
    }

    fn emit_matched_carry(&mut self, out: &mut Vec<u8>) {
        if is_prompt_line(&self.carry) {
            highlight_serial_prompt_line(&self.carry, out);
            self.carry.clear();
        }
    }

    /// Test hook: nothing buffered.
    #[cfg(test)]
    pub fn carry_is_empty(&self) -> bool {
        self.carry.is_empty()
    }
}

impl Default for PromptHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

/// Does the buffer already fully match a shell-prompt line? (Shared by the
/// streaming highlighter's early-emit path.)
fn is_prompt_line(line: &[u8]) -> bool {
    prompt_parts(line).is_some()
}

// ── Internal types ──────────────────────────────────────────────────────────

struct PromptParts {
    prefix: Option<Range<usize>>,
    user: Range<usize>,
    separator: Option<Range<usize>>,
    dir: Range<usize>,
    suffix: Option<Range<usize>>,
    sigil: Range<usize>,
}

// ── Per-line highlighting ───────────────────────────────────────────────────

fn highlight_serial_prompt_line(line: &[u8], out: &mut Vec<u8>) {
    let Some(parts) = prompt_parts(line) else {
        out.extend_from_slice(line);
        return;
    };
    // Match was found on ANSI-stripped clean text.
    // Build highlighted output from the clean text ranges.
    let clean = crate::ansi_strip::strip(line);
    if parts.sigil.end > clean.len() {
        out.extend_from_slice(line);
        return;
    }
    let content = &clean[..parts.sigil.end];
    let tail = &clean[parts.sigil.end..];

    // Emit original ANSI prefix (if any) before highlighting
    if let Some(prefix) = &parts.prefix {
        out.extend_from_slice(&content[prefix.clone()]);
    }
    out.extend_from_slice(GREEN_BOLD);
    out.extend_from_slice(&content[parts.user.clone()]);
    out.extend_from_slice(RESET);
    if let Some(sep) = &parts.separator {
        out.extend_from_slice(&content[sep.clone()]);
    }
    out.extend_from_slice(BLUE_BOLD);
    out.extend_from_slice(&content[parts.dir.clone()]);
    out.extend_from_slice(RESET);
    if let Some(suffix) = &parts.suffix {
        out.extend_from_slice(&content[suffix.clone()]);
    }
    out.extend_from_slice(WHITE_BOLD);
    out.extend_from_slice(&content[parts.sigil.clone()]);
    out.extend_from_slice(RESET);
    out.extend_from_slice(tail);
}

// ── Prompt detection ────────────────────────────────────────────────────────

fn prompt_parts(line: &[u8]) -> Option<PromptParts> {
    // Strip all ANSI via the vte-based stripper (src/ansi_strip.rs).
    let clean = crate::ansi_strip::strip(line);
    if clean.is_empty() {
        return None;
    }
    let end = clean
        .iter()
        .position(|&b| b == b'\r' || b == b'\n')
        .unwrap_or(clean.len());
    let text = std::str::from_utf8(&clean[..end]).ok()?;

    // Match prompt pattern on clean text
    parse_colon_prompt(text, &clean).or_else(|| parse_bracket_prompt(text, &clean))
}

// ── Colon format: `user@host:/path$ ` ───────────────────────────────────────

fn parse_colon_prompt(text: &str, line: &[u8]) -> Option<PromptParts> {
    static COLON_USER_RE: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
        fancy_regex::Regex::new(r"^[^/#\-\s]\S*\s?[^\s.:\[\]]*?(?=:\S[^\r\n$#]*[$#]\s)").unwrap()
    });
    static COLON_DIR_RE: LazyLock<fancy_regex::Regex> =
        LazyLock::new(|| fancy_regex::Regex::new(r"(?<=:)\S[^\r\n$#]*?(?=[$#]\s)").unwrap());
    static COLON_SIGIL_RE: LazyLock<fancy_regex::Regex> =
        LazyLock::new(|| fancy_regex::Regex::new(r"(?<=\S)[$#]\s").unwrap());
    let user = match_range(&COLON_USER_RE, text)?;
    let dir = match_range(&COLON_DIR_RE, text)?;
    let sigil = match_range(&COLON_SIGIL_RE, text)?;
    // Greedy \S* backtracking in COLON_USER_RE can end the user match after
    // COLON_DIR_RE's leftmost dir start (e.g. "12:30:45# cmd"), and an
    // earlier sigil can end before dir.end (e.g. "a$ x:y# ") — either would
    // make the separator slice reversed or the content slices out of range.
    if user.end > dir.start || dir.end > sigil.end {
        return None;
    }
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

// ── Bracket format: `[user@host /path]# ` ───────────────────────────────────

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
    // Greedy \s? in BRACKET_USER_RE can swallow the user/dir separator so
    // user.end lands after BRACKET_DIR_RE's leftmost dir start (e.g.
    // "[root@host /a /b]# cmd"), reversing the separator slice below.
    if user.end > dir.start {
        return None;
    }
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

// ── Regex helper ────────────────────────────────────────────────────────────

fn match_range(re: &fancy_regex::Regex, text: &str) -> Option<Range<usize>> {
    re.find(text).ok().flatten().map(|m| m.start()..m.end())
}

// ── Validation ──────────────────────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_highlight_colors_user_dir_and_sigil() {
        let mut out = Vec::new();
        highlight_serial_prompt(b"root@dut1:/# ip -c a\r\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("\x1b[1;32mroot@dut1\x1b[0m:"));
        assert!(rendered.contains("\x1b[1;34m/\x1b[0m"));
        assert!(rendered.contains("\x1b[1;37m# \x1b[0mip -c a"));
    }

    #[test]
    fn prompt_highlight_handles_bracket_prompt() {
        let mut out = Vec::new();
        highlight_serial_prompt(b"[root@dut1 ~]# pwd\r\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("[\x1b[1;32mroot@dut1\x1b[0m "));
        assert!(rendered.contains("\x1b[1;34m~\x1b[0m]"));
        assert!(rendered.contains("\x1b[1;37m# \x1b[0mpwd"));
    }

    #[test]
    fn prompt_highlight_short_colon_prompt() {
        // Regression: user@hostname directly followed by :/ without extra chars
        // e.g. root@board-name:/#
        let mut out = Vec::new();
        highlight_serial_prompt(b"root@board-name:/# ls\r\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("\x1b[1;32mroot@board-name\x1b[0m:"));
        assert!(rendered.contains("\x1b[1;34m/\x1b[0m"));
        assert!(rendered.contains("\x1b[1;37m# \x1b[0mls"));
    }

    #[test]
    fn prompt_highlight_bracketed_paste_prefix() {
        // real-world: \x1b[?2004hroot@board:/#
        // Leading ANSI escapes preserved as prefix
        let mut out = Vec::new();
        highlight_serial_prompt(b"\x1b[?2004hroot@board:/# ls\r\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("\x1b[1;32mroot@board\x1b[0m:"));
        assert!(rendered.contains("\x1b[1;32mroot@board\x1b[0m:"));
        assert!(rendered.contains("\x1b[1;34m/\x1b[0m"));
        assert!(rendered.contains("\x1b[1;37m# \x1b[0mls"));
    }

    /// Regression (field-observed): a prompt arriving SPLIT across
    /// chunk boundaries — e.g. right after an `ls` output burst — must
    /// still be highlighted. The stateless per-chunk call rendered it
    /// white because neither half matched the pattern alone.
    #[test]
    fn prompt_split_across_chunks_is_highlighted() {
        use super::PromptHighlighter;
        let t0 = std::time::Instant::now();
        let mut hl = PromptHighlighter::new();
        let mut out = Vec::new();
        // Chunk 1: ls output + the first half of the prompt (no newline).
        hl.highlight(b"ls\r\nsnap\r\n\x1b[?2004hroot@rk3576", t0, &mut out);
        // Chunk 2: the prompt tail, still no newline (within DEBOUNCE).
        hl.highlight(b":~# ", t0 + std::time::Duration::from_millis(1), &mut out);
        // Chunk 3: a command line completes it.
        hl.highlight(
            b"uptime\r\n",
            t0 + std::time::Duration::from_millis(2),
            &mut out,
        );
        let rendered = String::from_utf8(out).unwrap();
        assert!(
            rendered.contains("\x1b[1;32mroot@rk3576\x1b[0m"),
            "the split prompt is highlighted as ONE line: {rendered:?}"
        );
        assert!(
            rendered.contains("\x1b[1;34m~\x1b[0m"),
            "the dir part is highlighted: {rendered:?}"
        );
        // finish() flushes any buffered tail raw.
        let mut tail = Vec::new();
        hl.highlight(
            b"partial",
            t0 + std::time::Duration::from_millis(3),
            &mut tail,
        );
        hl.finish(&mut tail);
        assert!(tail.ends_with(b"partial"));
    }

    /// Typed ECHO must be visible WITHOUT Enter:
    /// a quiet partial line (non-prompt) is debounce-flushed raw. The
    /// bytes appear after DEBOUNCE of silence — never held until a
    /// newline.
    #[test]
    fn typed_echo_flushes_after_debounce_without_newline() {
        use super::PromptHighlighter;
        let t0 = std::time::Instant::now();
        let mut hl = PromptHighlighter::new();
        let mut out = Vec::new();
        // The idle prompt was emitted; the user types 'l' then 's'.
        hl.highlight(b"root@rk3576:~# ", t0, &mut out);
        hl.highlight(b"l", t0 + std::time::Duration::from_millis(100), &mut out);
        // Still inside the debounce window — nothing more emitted.
        hl.flush_quiet(t0 + std::time::Duration::from_millis(110), &mut out);
        assert!(!out.ends_with(b"l"), "inside DEBOUNCE the byte is held");
        // Quiet for >= DEBOUNCE — the echo flushes WITHOUT a newline.
        hl.highlight(b"s", t0 + std::time::Duration::from_millis(200), &mut out);
        hl.flush_quiet(t0 + std::time::Duration::from_millis(300), &mut out);
        assert!(out.ends_with(b"ls"), "echo visible after debounce: {out:?}");
    }

    /// A burst split ACROSS the debounce window still highlights: the
    /// second fragment resets nothing — as long as it arrives before the
    /// quiet flush, the merged line matches.
    #[test]
    fn burst_split_within_debounce_merges() {
        use super::PromptHighlighter;
        let t0 = std::time::Instant::now();
        let mut hl = PromptHighlighter::new();
        let mut out = Vec::new();
        hl.highlight(b"snap\r\nroot@rk3576", t0, &mut out);
        // NOT yet quiet for DEBOUNCE when the tail lands 20ms later.
        hl.flush_quiet(t0 + std::time::Duration::from_millis(10), &mut out);
        hl.highlight(b":~# ", t0 + std::time::Duration::from_millis(20), &mut out);
        let text = String::from_utf8(out.clone()).unwrap();
        assert!(
            text.contains("\x1b[1;32mroot@rk3576\x1b[0m"),
            "split prompt merged within DEBOUNCE is highlighted: {text:?}"
        );
    }

    /// Regression (live-board report): the IDLE prompt after an
    /// `ls` burst never sees a newline — it is the stream's last line. It
    /// must be highlighted the moment it already matches, not held white
    /// in the carry until the next command.
    #[test]
    fn idle_prompt_without_newline_is_highlighted_immediately() {
        use super::PromptHighlighter;
        let t0 = std::time::Instant::now();
        let mut hl = PromptHighlighter::new();
        let mut out = Vec::new();
        hl.highlight(b"snap\r\n\x1b[?2004h", t0, &mut out);
        // The prompt arrives complete (with trailing space) but NO newline.
        hl.highlight(
            b"root@rk3576:~# ",
            t0 + std::time::Duration::from_millis(1),
            &mut out,
        );
        assert!(
            out.windows(22)
                .any(|w| w == b"\x1b[1;32mroot@rk3576\x1b[0m"),
            "the idle prompt is highlighted immediately: {out:?}"
        );
        assert!(hl.carry_is_empty());
        // A later command echo appends as its own plain segment.
        let mut more = Vec::new();
        hl.highlight(
            b"uptime\r\n",
            t0 + std::time::Duration::from_millis(2),
            &mut more,
        );
        assert!(more.starts_with(b"uptime"));
    }

    #[test]
    fn prompt_highlight_does_not_color_non_prompt_lines() {
        let mut out = Vec::new();
        highlight_serial_prompt(b"[    0.000] Booting Linux\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();
        // No ANSI codes should be present
        assert!(!rendered.contains("\x1b["));
    }
}
