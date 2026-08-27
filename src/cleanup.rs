//! Cleaning up an LLM's raw response before it's used as the commit message (prd.md
//! "Response cleanup"). Pure. Applied once, in `generation::generate`, right after the
//! backend call returns and before the blank check — so it reaches both `--dry-run`'s
//! stdout print and the `$EDITOR` pre-population the same way, and neither has to
//! reimplement it. Always trims leading/trailing whitespace, on top of unwrapping a
//! code fence or quoted string when either is present.

/// Trims `raw` of leading/trailing whitespace, then strips a wrapping Markdown code
/// fence, then a wrapping pair of quotation marks — unconditionally: the result is
/// always at least `raw.trim()`, even when neither the fence nor the quote-wrap
/// applies, so no incidental surrounding whitespace ever reaches `--dry-run`'s stdout,
/// the message review prompt, or `$EDITOR`'s pre-population.
#[must_use]
pub fn clean_message(raw: &str) -> String {
    let trimmed = raw.trim();
    let defenced = strip_code_fence(trimmed);
    strip_surrounding_quotes(&defenced)
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
pub fn strip_code_fence(msg: &str) -> String {
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
pub fn strip_surrounding_quotes(msg: &str) -> String {
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
}
