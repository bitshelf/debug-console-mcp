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
    let clean = strip_ansi_escapes::strip(line);
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
    // Strip all ANSI using the well-tested strip-ansi-escapes crate
    let clean = strip_ansi_escapes::strip(line);
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
    let separator = user.end..dir.start;
    let suffix = dir.end..sigil.start;
    if !line[separator.clone()].iter().all(|b| b.is_ascii_whitespace())
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
        highlight_serial_prompt(b"root@myd-lt527:/# ip -c a\r\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();
        assert!(rendered.contains("\x1b[1;32mroot@myd-lt527\x1b[0m:"));
        assert!(rendered.contains("\x1b[1;34m/\x1b[0m"));
        assert!(rendered.contains("\x1b[1;37m# \x1b[0mip -c a"));
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

    #[test]
    fn prompt_highlight_does_not_color_non_prompt_lines() {
        let mut out = Vec::new();
        highlight_serial_prompt(b"[    0.000] Booting Linux\n", &mut out);
        let rendered = String::from_utf8(out).unwrap();
        // No ANSI codes should be present
        assert!(!rendered.contains("\x1b["));
    }
}
