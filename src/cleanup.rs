//! Cleaning up an LLM's raw response before it's used as the commit message (prd.md
//! "Response cleanup"). Pure. Applied once, in `generation::generate`, right after the
//! backend call returns and before the blank check — so it reaches both `--dry-run`'s
//! stdout print and the `$EDITOR` pre-population the same way, and neither has to
//! reimplement it. Always strips terminal escape sequences/stray control characters and
//! trims leading/trailing whitespace, on top of unwrapping a code fence or quoted string
//! when either is present.

/// Strips terminal escape sequences and stray control characters from `raw`, trims it
/// of leading/trailing whitespace, then strips a wrapping Markdown code fence, then a
/// wrapping pair of quotation marks — unconditionally: the result is always at least
/// `strip_terminal_escapes(raw).trim()`, even when neither the fence nor the quote-wrap
/// applies, so no incidental surrounding whitespace or terminal control codes ever reach
/// `--dry-run`'s stdout, the message review prompt, or `$EDITOR`'s pre-population.
#[must_use]
pub fn clean_message(raw: &str) -> String {
    let unescaped = strip_terminal_escapes(raw);
    let trimmed = unescaped.trim();
    let defenced = strip_code_fence(trimmed);
    strip_surrounding_quotes(&defenced)
}

const ESC: char = '\u{1b}';
const BEL: char = '\u{07}';

/// Removes terminal escape sequences and stray control characters. Some agent CLIs
/// write cursor/color control codes (e.g. `ESC[?25h` to re-show the cursor) to stdout
/// even when it's a pipe, which would otherwise end up in the commit message — and
/// would defeat the fence/quote detection and blank check that follow, since the
/// message wouldn't start/end with the expected characters or be truly blank.
///
/// Recognized and dropped in full:
/// - CSI sequences: `ESC [`, parameter bytes `0x30–0x3F`, intermediate bytes
///   `0x20–0x2F`, one final byte `0x40–0x7E`.
/// - String sequences — OSC (`ESC ]`), DCS (`ESC P`), SOS (`ESC X`), PM (`ESC ^`), APC
///   (`ESC _`): everything up to and including the string terminator `ESC \` or BEL.
/// - Any other escape sequence: `ESC`, intermediate bytes `0x20–0x2F`, one final byte
///   `0x30–0x7E` (e.g. `ESC ( B`, `ESC =`, `ESC 7`).
/// - Any other C0 control (except `\n` and `\t`), DEL, or C1 control character, one
///   character at a time — including `\r`, so CRLF line endings become plain LF.
///
/// 8-bit C1 characters (`U+0080–U+009F`) are deliberately *not* parsed as sequence
/// introducers (UTF-8 terminals don't treat them as such either): in practice they
/// come from mojibake (e.g. `”` mis-decoded as Latin-1 ends in `U+009D`), and treating
/// one as an unterminated OSC/CSI would swallow real message text after it. A lone
/// `ESC` not followed by a recognized sequence is dropped by itself, and whatever
/// follows is processed normally. An unterminated sequence at the end of the input is
/// dropped through end of input.
#[must_use]
fn strip_terminal_escapes(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != ESC {
            push_if_printable(&mut out, c);
            continue;
        }
        if chars.next_if_eq(&'[').is_some() {
            skip_csi(&mut chars);
        } else if chars
            .next_if(|c| matches!(c, ']' | 'P' | 'X' | '^' | '_'))
            .is_some()
        {
            skip_string(&mut chars);
        } else {
            // Intermediate bytes (if any), then one final byte (`ESC ( B`, `ESC 7`, ...).
            // If neither follows, this was a lone ESC and only it is dropped.
            skip_while_in(&mut chars, '\u{20}'..='\u{2f}');
            chars.next_if(|c| ('\u{30}'..='\u{7e}').contains(c));
        }
    }
    out
}

/// Keeps `c` unless it's a control character other than `\n`/`\t`.
fn push_if_printable(out: &mut String, c: char) {
    if c == '\n' || c == '\t' || !c.is_control() {
        out.push(c);
    }
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// Consumes characters while they fall within `range`.
fn skip_while_in(chars: &mut Chars<'_>, range: std::ops::RangeInclusive<char>) {
    while chars.next_if(|c| range.contains(c)).is_some() {}
}

/// Skips the remainder of a CSI sequence (after its `ESC [` introducer).
fn skip_csi(chars: &mut Chars<'_>) {
    skip_while_in(chars, '\u{30}'..='\u{3f}');
    skip_while_in(chars, '\u{20}'..='\u{2f}');
    chars.next_if(|c| ('\u{40}'..='\u{7e}').contains(c));
}

/// Skips the remainder of an OSC/DCS/SOS/PM/APC string (after its introducer), through
/// its terminator: `ESC \` or BEL.
fn skip_string(chars: &mut Chars<'_>) {
    while let Some(c) = chars.next() {
        match c {
            BEL => return,
            ESC if chars.next_if_eq(&'\\').is_some() => return,
            _ => {}
        }
    }
}

/// Strips a single Markdown code fence wrapping the whole message. Some LLMs answer with
/// the commit message inside a ```…``` block; when the first line is a bare opening fence
/// — a run of at least 3 backticks, optionally followed by a language tag, and nothing
/// else (a line with text after the backticks, e.g. "```feat: add x", is the message
/// itself, not a wrapper) — and the first following line that's a bare closing fence — a
/// run of at least as many backticks, and nothing else — is the message's very last line,
/// both are dropped. If a closing fence appears before the last line, the message holds
/// other content or additional code blocks rather than one wrapping fence, so it's
/// returned unchanged; same if there's no closing fence at all, or the message isn't
/// fenced to begin with.
#[must_use]
fn strip_code_fence(msg: &str) -> String {
    let lines: Vec<&str> = msg.split('\n').collect();
    if lines.len() < 2 {
        return msg.to_string();
    }
    let Some(fence_len) = opening_fence_len(lines[0]) else {
        return msg.to_string();
    };
    for (i, line) in lines.iter().enumerate().skip(1) {
        if is_closing_fence(line, fence_len) {
            return if i == lines.len() - 1 {
                lines[1..i].join("\n").trim().to_string()
            } else {
                msg.to_string()
            };
        }
    }
    msg.to_string()
}

/// The backtick-run length if `line` is a bare opening fence: optional leading
/// whitespace, a run of at least 3 backticks, then optionally more whitespace and a
/// language tag (letters, digits, `_`, `+`, `-`), then optional trailing whitespace, and
/// nothing else.
fn opening_fence_len(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let backticks = trimmed.chars().take_while(|&c| c == '`').count();
    if backticks < 3 {
        return None;
    }
    let rest = trimmed[backticks..].trim_start();
    let tag_end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '+' || c == '-'))
        .unwrap_or(rest.len());
    rest[tag_end..].trim().is_empty().then_some(backticks)
}

/// Whether `line` is a bare closing fence: optional whitespace, a run of at least
/// `fence_len` backticks, then optional whitespace, and nothing else.
fn is_closing_fence(line: &str, fence_len: usize) -> bool {
    let trimmed = line.trim();
    let backticks = trimmed.chars().take_while(|&c| c == '`').count();
    backticks >= fence_len && trimmed[backticks..].trim().is_empty()
}

/// Quotation-mark pairs recognized by `strip_surrounding_quotes`: straight quotes (`"`,
/// `'`, `` ` ``) and typographic "smart" quotes.
const QUOTE_PAIRS: [(&str, &str); 5] = [
    ("\"", "\""),
    ("'", "'"),
    ("`", "`"),
    ("\u{201c}", "\u{201d}"), // “ ”
    ("\u{2018}", "\u{2019}"), // ‘ ’
];

/// Strips a single pair of matching quotation marks wrapping `text`, returning the
/// unwrapped inner text — but only when the pair is genuinely enclosing: `text` starts
/// with `open` and ends with `close`, and neither delimiter reappears in what's between
/// them. So an apostrophe within the text (e.g. `fix: don't crash`), or two separate
/// quoted spans (e.g. `"foo" and "bar"`), are left alone.
fn unwrap_quotes(text: &str) -> Option<&str> {
    for (open, close) in QUOTE_PAIRS {
        if text.len() >= open.len() + close.len() && text.starts_with(open) && text.ends_with(close)
        {
            let inner = &text[open.len()..text.len() - close.len()];
            if !inner.contains(open) && !inner.contains(close) {
                return Some(inner);
            }
        }
    }
    None
}

/// Strips a single pair of matching quotation marks wrapping the whole message, or —
/// failing that — just its first line (a quoted subject with an unquoted body). See
/// `unwrap_quotes` for which pairs are recognized and what counts as "genuinely
/// enclosing". Returns the message unchanged when neither wrap applies.
#[must_use]
fn strip_surrounding_quotes(msg: &str) -> String {
    if let Some(whole) = unwrap_quotes(msg.trim()) {
        return whole.trim().to_string();
    }
    let mut lines: Vec<&str> = msg.split('\n').collect();
    let Some(subject) = lines.first().copied().and_then(unwrap_quotes) else {
        return msg.to_string();
    };
    let trimmed_subject = subject.trim().to_string();
    lines[0] = trimmed_subject.as_str();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_message_trims_a_plain_message_with_no_fence_or_quotes() {
        // No fence, no quotes — but the trailing newline is still stripped
        // unconditionally, not left as a side effect of some other transformation.
        assert_eq!(clean_message("feat: add x\n"), "feat: add x");
    }

    #[test]
    fn clean_message_trims_leading_and_trailing_whitespace_with_nothing_else_to_clean() {
        assert_eq!(clean_message("  feat: add x  \n"), "feat: add x");
    }

    #[test]
    fn clean_message_strips_a_fenced_block_with_a_language_tag() {
        assert_eq!(clean_message("```text\nfeat: add x\n```\n"), "feat: add x");
    }

    #[test]
    fn clean_message_strips_a_fenced_block_with_no_language_tag() {
        assert_eq!(clean_message("```\nfeat: add x\n```"), "feat: add x");
    }

    #[test]
    fn clean_message_strips_a_multiline_fenced_block() {
        assert_eq!(
            clean_message("```\nfeat: add x\n\n- did a thing\n```\n"),
            "feat: add x\n\n- did a thing"
        );
    }

    #[test]
    fn clean_message_leaves_a_fence_unchanged_when_the_close_is_not_the_last_line() {
        let msg = "```\nfeat: add x\n```\ntrailing text";
        assert_eq!(clean_message(msg), msg);
    }

    #[test]
    fn clean_message_leaves_a_fence_unchanged_when_there_is_no_closing_fence() {
        let msg = "```\nfeat: add x";
        assert_eq!(clean_message(msg), msg);
    }

    #[test]
    fn clean_message_leaves_a_first_line_with_text_after_the_backticks_unchanged() {
        let msg = "```feat: add x\nmore\n```";
        assert_eq!(clean_message(msg), msg);
    }

    #[test]
    fn clean_message_strips_surrounding_double_quotes_on_the_whole_message() {
        assert_eq!(clean_message("\"feat: add x\"\n"), "feat: add x");
    }

    #[test]
    fn clean_message_strips_surrounding_smart_quotes() {
        assert_eq!(clean_message("\u{201c}feat: add x\u{201d}"), "feat: add x");
    }

    #[test]
    fn clean_message_strips_a_quoted_subject_leaving_the_body_alone() {
        assert_eq!(
            clean_message("\"feat: add x\"\n\n- did a thing"),
            "feat: add x\n\n- did a thing"
        );
    }

    #[test]
    fn clean_message_leaves_an_apostrophe_within_the_text_alone() {
        let msg = "fix: don't crash";
        assert_eq!(clean_message(msg), msg);
    }

    #[test]
    fn clean_message_leaves_two_separate_quoted_spans_alone() {
        let msg = "\"foo\" and \"bar\"";
        assert_eq!(clean_message(msg), msg);
    }

    #[test]
    fn clean_message_strips_both_a_fence_and_quotes_inside_it() {
        assert_eq!(clean_message("```\n\"feat: add x\"\n```"), "feat: add x");
    }

    #[test]
    fn clean_message_of_an_all_whitespace_response_collapses_to_blank() {
        // The unconditional trim collapses this to "" itself now — the caller's blank
        // check (`message.trim().is_empty()`) still catches it either way (exit 15), but
        // it no longer needs to see the raw whitespace to do so.
        assert_eq!(clean_message("   \n\t\n"), "");
    }

    #[test]
    fn clean_message_of_an_empty_fenced_block_is_blank() {
        assert_eq!(clean_message("```\n```"), "");
    }

    #[test]
    fn clean_message_strips_cursor_control_csi_sequences() {
        assert_eq!(clean_message("\x1b[?12l\x1b[?25hhi"), "hi");
    }

    #[test]
    fn clean_message_strips_escapes_before_fence_and_quote_detection() {
        assert_eq!(
            clean_message("\x1b[?25l```\nfeat: add x\n```\x1b[?25h\n"),
            "feat: add x"
        );
        assert_eq!(
            clean_message("\x1b[1m\"feat: add x\"\x1b[0m"),
            "feat: add x"
        );
    }

    #[test]
    fn clean_message_strips_a_fence_from_a_crlf_response() {
        assert_eq!(clean_message("```\r\nfeat: x\r\n```\r\n"), "feat: x");
    }

    #[test]
    fn clean_message_of_only_escape_sequences_is_blank() {
        assert_eq!(clean_message("\x1b[?12l\x1b[?25h\n"), "");
    }

    #[test]
    fn strip_terminal_escapes_removes_sgr_colors_within_text() {
        assert_eq!(
            strip_terminal_escapes("feat: \x1b[32madd\x1b[0m x\n\n- \x1b[1;31mbody\x1b[m"),
            "feat: add x\n\n- body"
        );
    }

    #[test]
    fn strip_terminal_escapes_removes_osc_sequences_with_either_terminator() {
        assert_eq!(strip_terminal_escapes("\x1b]0;title\x07hi"), "hi");
        assert_eq!(
            strip_terminal_escapes("\x1b]8;;https://x.y\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
    }

    #[test]
    fn strip_terminal_escapes_removes_dcs_and_apc_strings() {
        assert_eq!(
            strip_terminal_escapes("a\x1bPq#0\x1b\\b\x1b_apc\x1b\\c"),
            "abc"
        );
    }

    #[test]
    fn strip_terminal_escapes_removes_other_escape_sequences() {
        // Charset designation, keypad mode, save/restore cursor.
        assert_eq!(strip_terminal_escapes("\x1b(Ba\x1b=b\x1b7c\x1b8"), "abc");
    }

    #[test]
    fn strip_terminal_escapes_drops_8bit_c1_chars_one_at_a_time_without_eating_text() {
        // Mojibake: `”` (E2 80 9D) and `ś` (C5 9B) decoded as Latin-1. Each C1 char is
        // dropped by itself rather than starting an OSC/CSI that swallows what follows.
        assert_eq!(
            strip_terminal_escapes("fix: \u{e2}\u{80}\u{9d}rest of message"),
            "fix: \u{e2}rest of message"
        );
        assert_eq!(strip_terminal_escapes("\u{c5}\u{9b}1m ok"), "\u{c5}1m ok");
    }

    #[test]
    fn strip_terminal_escapes_parses_a_sequence_right_after_a_lone_esc() {
        assert_eq!(strip_terminal_escapes("\x1b\x1b[31mhi"), "hi");
        assert_eq!(strip_terminal_escapes("a\x1b\nb"), "a\nb");
        assert_eq!(strip_terminal_escapes("a\x1b\u{e9}b"), "a\u{e9}b");
    }

    #[test]
    fn strip_terminal_escapes_removes_stray_controls_but_keeps_newlines_and_tabs() {
        assert_eq!(
            strip_terminal_escapes("a\r\nb\x07\x08\x7f\tc\x00"),
            "a\nb\tc"
        );
    }

    #[test]
    fn strip_terminal_escapes_drops_an_unterminated_sequence_through_end_of_input() {
        assert_eq!(strip_terminal_escapes("hi\x1b]0;never ends"), "hi");
        assert_eq!(strip_terminal_escapes("hi\x1b[12"), "hi");
        assert_eq!(strip_terminal_escapes("hi\x1b"), "hi");
    }

    #[test]
    fn strip_terminal_escapes_leaves_ordinary_unicode_alone() {
        let msg = "fix: handle \u{201c}café\u{201d} — ✓ 日本語";
        assert_eq!(strip_terminal_escapes(msg), msg);
    }
}
