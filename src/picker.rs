//! Plain stdin pickers (prd.md, "jj commit commands"; "Selection"; "Message review
//! prompt"): the jj commit-command picker (never more than two choices), default
//! mode's tool picker (an arbitrary number of `api.yaml` entries, shown only when the
//! choice is ambiguous), and the message review prompt (regenerate/edit/accept, shown
//! after every generation) — all plain stdin prompts, so no fuzzy-finder or TUI crate
//! is warranted (see "Preferences of Dependencies" #3).

use crate::error::{CcmError, PickerCancelled};
use crate::term;
use crate::vcs::argv::JjCommitCommand;
use std::io::{BufRead, Write};

/// The two choices offered, in order: index 0 is always `jj commit`; index 1 is `jj
/// split` when `--include`/`--exclude` was given (`scoped`), or `jj describe`
/// otherwise. Exactly one of `describe`/`split` is ever offered — never both, never
/// neither.
#[must_use]
pub fn choices(scoped: bool) -> [JjCommitCommand; 2] {
    if scoped {
        [JjCommitCommand::Commit, JjCommitCommand::Split]
    } else {
        [JjCommitCommand::Commit, JjCommitCommand::Describe]
    }
}

fn label(cmd: JjCommitCommand) -> &'static str {
    match cmd {
        JjCommitCommand::Commit => "jj commit",
        JjCommitCommand::Describe => "jj describe",
        JjCommitCommand::Split => "jj split",
    }
}

/// One line of picker input, already interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerInput {
    Chosen(usize),
    Retry,
}

/// Interprets one line of picker input: trimmed of leading/trailing whitespace (so a
/// trailing `\r` from a CRLF terminal, or a stray leading/trailing space, doesn't turn
/// a valid selection into an invalid one), then exactly `"0"` or `"1"` selects that
/// index; anything else — including a blank or all-whitespace line — is a retry.
#[must_use]
pub fn interpret(line: &str) -> PickerInput {
    match line.trim() {
        "0" => PickerInput::Chosen(0),
        "1" => PickerInput::Chosen(1),
        _ => PickerInput::Retry,
    }
}

/// Why [`prompt`] didn't return a selection.
#[derive(Debug, thiserror::Error)]
pub enum PickerError {
    /// True EOF (Ctrl-D) without ever selecting a valid index — prd.md's "the user
    /// canceled the jj-command picker" (exit 14).
    #[error("aborted: no jj command selected")]
    Cancelled,
    /// A genuine I/O error reading a line (e.g. invalid UTF-8 on stdin) — distinct
    /// from EOF and not itself a "the user cancelled" signal, so callers should not
    /// report it the same way.
    #[error("failed to read picker input: {0}")]
    Io(#[source] std::io::Error),
}

/// Runs the picker to a selection: prints the two `choices` to `writer` as `0) jj
/// commit` / `1) jj describe|split` (marking whichever index `default` names with a
/// trailing `  (default)`), reads a line from `reader`, and repeats on any invalid
/// (non-`"0"`/`"1"`) input — except a blank or all-whitespace line, which selects
/// `choices[default]` immediately when `default` is `Some`. True EOF is unaffected by
/// `default`: it's checked first and always cancels, even with a default set, since it
/// signals a closed/non-interactive stdin rather than the user pressing Enter.
///
/// # Errors
/// [`PickerError::Cancelled`] on EOF without ever selecting a valid index;
/// [`PickerError::Io`] if reading a line fails for a reason other than EOF.
pub fn prompt(
    reader: &mut (impl BufRead + ?Sized),
    writer: &mut (impl Write + ?Sized),
    choices: &[JjCommitCommand; 2],
    default: Option<usize>,
) -> Result<JjCommitCommand, PickerError> {
    loop {
        for (index, choice) in choices.iter().enumerate() {
            let suffix = if default == Some(index) {
                "  (default)"
            } else {
                ""
            };
            let _ = writeln!(writer, "{index}) {}{suffix}", label(*choice));
        }

        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).map_err(PickerError::Io)?;
        if bytes_read == 0 {
            return Err(PickerError::Cancelled);
        }

        match interpret(&line) {
            PickerInput::Chosen(index) => return Ok(choices[index]),
            PickerInput::Retry => {
                if let Some(d) = default
                    && line.trim().is_empty()
                {
                    return Ok(choices[d]);
                }
            }
        }
    }
}

/// Maps a [`PickerError`] to its documented exit code: true EOF without a selection is
/// [`PickerCancelled`] (exit 14); any other error (e.g. invalid UTF-8 on stdin) is the
/// generic exit-1 catch-all. Shared by every picker caller (the jj commit-command picker
/// in `commit.rs`, the tool picker in `pipeline.rs`) so the two variants
/// are never mapped inconsistently.
#[must_use]
pub fn to_ccm_error(err: PickerError) -> CcmError {
    if matches!(err, PickerError::Cancelled) {
        CcmError::from(PickerCancelled)
    } else {
        CcmError::Unexpected(err.to_string())
    }
}

/// Interprets one line of tool-picker input: trimmed, then accepted as a
/// selection only if it parses as a `usize` strictly less than `count`; anything else —
/// including a blank line, a negative number, or an out-of-range index — is a retry.
#[must_use]
pub fn interpret_index(line: &str, count: usize) -> PickerInput {
    match line.trim().parse::<usize>() {
        Ok(index) if index < count => PickerInput::Chosen(index),
        _ => PickerInput::Retry,
    }
}

/// Default mode's tool picker: an arbitrary-length numbered-menu variant of
/// [`prompt`]. Prints each of `lines` as `N) <line>` (the `(default)` marker, if any, is
/// already baked into the relevant line by `config::listing::lines` — this function adds
/// no marker of its own), then a `Select a tool [0-N]: ` prompt with no trailing newline
/// (flushed so it's visible before `reader` blocks), and reprints the whole menu on any
/// invalid input — except a blank or all-whitespace line, which selects `default`
/// immediately when it's `Some`. Same EOF-is-[`PickerError::Cancelled`] (unaffected by
/// `default`, same reasoning as [`prompt`]) / IO-error-is-[`PickerError::Io`] split as
/// [`prompt`]. `lines` must be non-empty.
///
/// # Errors
/// [`PickerError::Cancelled`] on EOF without ever selecting a valid index;
/// [`PickerError::Io`] if reading a line fails for a reason other than EOF.
pub fn prompt_index(
    reader: &mut (impl BufRead + ?Sized),
    writer: &mut (impl Write + ?Sized),
    lines: &[String],
    default: Option<usize>,
) -> Result<usize, PickerError> {
    let last = lines.len().saturating_sub(1);
    loop {
        for (index, line) in lines.iter().enumerate() {
            let _ = writeln!(writer, "{index}) {line}");
        }
        let _ = write!(writer, "Select a tool [0-{last}]: ");
        let _ = writer.flush();

        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).map_err(PickerError::Io)?;
        if bytes_read == 0 {
            return Err(PickerError::Cancelled);
        }

        match interpret_index(&line, lines.len()) {
            PickerInput::Chosen(index) => return Ok(index),
            PickerInput::Retry => {
                if let Some(d) = default
                    && line.trim().is_empty()
                {
                    return Ok(d);
                }
            }
        }
    }
}

/// One action selected from the message review prompt (prd.md "Message review
/// prompt"): [`ReviewAction::Regenerate`] re-runs stage 7 (generation) against the
/// already-selected entry and already-computed diff; [`ReviewAction::Edit`] opens
/// `$EDITOR` on the current message, same as today's fixed behavior;
/// [`ReviewAction::Accept`] commits the current message as-is, skipping `$EDITOR`
/// entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAction {
    Regenerate,
    Edit,
    Accept,
}

/// Interprets one byte of review-prompt input: `r`/`R` is always
/// [`ReviewAction::Regenerate`]; `e`/`E` is always [`ReviewAction::Edit`]; a space or
/// Enter (`\n`/`\r`) is the prompt's default action — [`ReviewAction::Accept`] when
/// `allow_accept` (a non-blank generated message), or [`ReviewAction::Edit`] otherwise,
/// since a blank generation has nothing worth accepting (see "Message review prompt").
/// Any other byte is a retry (`None`).
///
/// Ctrl-D (`0x04`) is deliberately not classified here: unlike every other
/// unrecognized byte, it doesn't mean "keep asking, try again" — it cancels the prompt
/// outright, which is [`prompt_action`]'s job, not this pure classifier's.
#[must_use]
pub fn interpret_action(byte: u8, allow_accept: bool) -> Option<ReviewAction> {
    match byte {
        b'r' | b'R' => Some(ReviewAction::Regenerate),
        b'e' | b'E' => Some(ReviewAction::Edit),
        b' ' | b'\n' | b'\r' => Some(if allow_accept {
            ReviewAction::Accept
        } else {
            ReviewAction::Edit
        }),
        _ => None,
    }
}

fn print_action_menu(writer: &mut (impl Write + ?Sized), allow_accept: bool) {
    if allow_accept {
        let _ = write!(writer, "[R]egenerate  [E]dit  [Space/Enter] accept: ");
    } else {
        let _ = write!(writer, "[R]egenerate  [Space/Enter] edit: ");
    }
    let _ = writer.flush();
}

/// Reads and discards the remainder of the current line (up to and including the
/// terminating `\n`, or EOF), one byte at a time. Only used in line mode (`raw ==
/// false` in [`prompt_action`]): since a non-terminal stdin delivers a whole typed line
/// at once regardless of which single byte we act on, this keeps every attempt —
/// whether it selected a valid action or was a retry — consuming exactly one line, the
/// same granularity [`prompt`]/[`prompt_index`] read at via `read_line`. Without this,
/// a leftover `\n` (or trailing garbage) would be mistaken for the next prompt's own
/// input, be it another attempt at this same prompt or the jj commit-command picker.
fn drain_rest_of_line(
    reader: &mut (impl BufRead + ?Sized),
    first_byte: u8,
) -> Result<(), PickerError> {
    if first_byte == b'\n' {
        return Ok(());
    }
    let mut buf = [0u8; 1];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(()), // EOF mid-line: nothing left to drain.
            Ok(_) => {
                if buf[0] == b'\n' {
                    return Ok(());
                }
            }
            Err(err) => return Err(PickerError::Io(err)),
        }
    }
}

/// Runs the message review prompt (prd.md "Message review prompt") to a selection:
/// prints `[R]egenerate  [E]dit  [Space/Enter] accept: ` (or, when `allow_accept` is
/// `false` — the generated message is blank — `[R]egenerate  [Space/Enter] edit: `),
/// reads a single byte, and repeats on any input [`interpret_action`] doesn't
/// recognize.
///
/// When `raw` is `true`, stdin is put into raw terminal mode ([`term::RawGuard`]) for
/// the read, so a single keypress selects an action with no Enter needed — the caller
/// (`pipeline::run`) only passes `true` when `Environment::stdin_is_terminal` reports a
/// real terminal, and `reader` is that same terminal's stdin in that case. When raw
/// mode isn't actually engaged for the read — either `raw` is `false` (stdin isn't a
/// real terminal — every integration test, and any piped caller), or `raw` is `true`
/// but [`term::RawGuard::enable`] itself fails despite stdin reporting as a terminal —
/// only the first byte of each line is inspected, and [`drain_rest_of_line`] discards
/// the remainder — up to and including the terminating `\n`, or EOF — on every attempt,
/// valid or not, so the next read of `reader` (a retry of this same prompt, or the jj
/// commit-command picker) always starts at a clean line boundary; skipping this drain
/// is only correct when raw mode genuinely suppressed canonical line buffering. A
/// newline is written after a valid selection either way, since raw mode echoes
/// nothing back to the terminal on its own.
///
/// EOF (zero bytes read) or a literal Ctrl-D byte (`0x04`) cancels immediately,
/// regardless of `raw` — the same "the user didn't finish choosing" signal the other
/// pickers treat as [`PickerError::Cancelled`].
///
/// # Errors
/// [`PickerError::Cancelled`] on EOF or Ctrl-D before a valid selection;
/// [`PickerError::Io`] if reading a byte fails for a reason other than EOF.
pub fn prompt_action(
    reader: &mut (impl BufRead + ?Sized),
    writer: &mut (impl Write + ?Sized),
    allow_accept: bool,
    raw: bool,
) -> Result<ReviewAction, PickerError> {
    loop {
        print_action_menu(writer, allow_accept);

        let mut buf = [0u8; 1];
        // Whether raw mode was actually engaged, not merely requested: if `raw` is
        // true but `RawGuard::enable` itself fails (e.g. `tcgetattr`/`tcsetattr` erroring
        // despite stdin reporting as a terminal), the terminal stays in canonical
        // line-buffered mode, and skipping the drain below on `raw` alone would leave a
        // trailing `\n` (or more) unconsumed, leaking into the next read.
        let (bytes_read, raw_engaged) = {
            let guard = raw
                .then(|| term::RawGuard::enable(std::io::stdin()))
                .flatten();
            let raw_engaged = guard.is_some();
            (reader.read(&mut buf).map_err(PickerError::Io)?, raw_engaged)
        };
        if bytes_read == 0 {
            return Err(PickerError::Cancelled);
        }
        let byte = buf[0];

        if !raw_engaged {
            drain_rest_of_line(reader, byte)?;
        }

        if byte == 0x04 {
            return Err(PickerError::Cancelled);
        }

        if let Some(action) = interpret_action(byte, allow_accept) {
            let _ = writeln!(writer);
            return Ok(action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    #[test]
    fn scoped_offers_commit_and_split() {
        assert_eq!(
            choices(true),
            [JjCommitCommand::Commit, JjCommitCommand::Split]
        );
    }

    #[test]
    fn unscoped_offers_commit_and_describe() {
        assert_eq!(
            choices(false),
            [JjCommitCommand::Commit, JjCommitCommand::Describe]
        );
    }

    #[test]
    fn interpret_exact_zero_and_one() {
        assert_eq!(interpret("0"), PickerInput::Chosen(0));
        assert_eq!(interpret("1"), PickerInput::Chosen(1));
    }

    #[test]
    fn interpret_trims_whitespace_and_crlf() {
        assert_eq!(interpret(" 1 \r\n"), PickerInput::Chosen(1));
        assert_eq!(interpret("\t0\t"), PickerInput::Chosen(0));
    }

    #[test]
    fn interpret_anything_else_is_a_retry() {
        assert_eq!(interpret(""), PickerInput::Retry);
        assert_eq!(interpret("   "), PickerInput::Retry);
        assert_eq!(interpret("2"), PickerInput::Retry);
        assert_eq!(interpret("y"), PickerInput::Retry);
    }

    #[test]
    fn prompt_selects_a_valid_first_answer() {
        let mut input = Cursor::new(b"0\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt(&mut input, &mut output, &choices(false), None).unwrap();
        assert_eq!(picked, JjCommitCommand::Commit);
        let printed = String::from_utf8(output).unwrap();
        assert_eq!(printed, "0) jj commit\n1) jj describe\n");
    }

    #[test]
    fn prompt_selects_index_one() {
        let mut input = Cursor::new(b"1\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt(&mut input, &mut output, &choices(true), None).unwrap();
        assert_eq!(picked, JjCommitCommand::Split);
    }

    #[test]
    fn prompt_reprints_choices_and_retries_on_invalid_input() {
        // With no default set, a blank line is just as invalid as "x" — both retry.
        let mut input = Cursor::new(b"x\n\n0\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt(&mut input, &mut output, &choices(false), None).unwrap();
        assert_eq!(picked, JjCommitCommand::Commit);
        let printed = String::from_utf8(output).unwrap();
        // Printed once per attempt: invalid "x", blank, then the winning "0" — three
        // reprints of the two-line menu.
        assert_eq!(printed.matches("0) jj commit").count(), 3);
    }

    #[test]
    fn prompt_blank_input_selects_the_given_default() {
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt(&mut input, &mut output, &choices(true), Some(1)).unwrap();
        assert_eq!(picked, JjCommitCommand::Split);
    }

    #[test]
    fn prompt_blank_input_without_a_default_still_retries() {
        let mut input = Cursor::new(b"\n0\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt(&mut input, &mut output, &choices(false), None).unwrap();
        assert_eq!(picked, JjCommitCommand::Commit);
        let printed = String::from_utf8(output).unwrap();
        assert_eq!(printed.matches("0) jj commit").count(), 2);
    }

    #[test]
    fn prompt_marks_the_default_choice_in_the_menu() {
        let mut input = Cursor::new(b"0\n".to_vec());
        let mut output = Vec::new();
        prompt(&mut input, &mut output, &choices(false), Some(0)).unwrap();
        let printed = String::from_utf8(output).unwrap();
        assert_eq!(printed, "0) jj commit  (default)\n1) jj describe\n");
    }

    #[test]
    fn prompt_prints_no_default_marker_when_default_is_none() {
        let mut input = Cursor::new(b"0\n".to_vec());
        let mut output = Vec::new();
        prompt(&mut input, &mut output, &choices(false), None).unwrap();
        let printed = String::from_utf8(output).unwrap();
        assert!(!printed.contains("(default)"));
    }

    #[test]
    fn prompt_cancels_on_eof_without_a_valid_selection() {
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();
        let result = prompt(&mut input, &mut output, &choices(false), None);
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn prompt_eof_still_cancels_even_with_a_default_set() {
        // True EOF (0 bytes read) is checked before any blank-line/default handling, so
        // it must still cancel even when a default is available — EOF signals a closed/
        // non-interactive stdin, not the user pressing Enter.
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();
        let result = prompt(&mut input, &mut output, &choices(false), Some(0));
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn prompt_cancels_on_eof_after_some_invalid_attempts() {
        let mut input = Cursor::new(b"garbage\n".to_vec());
        let mut output = Vec::new();
        let result = prompt(&mut input, &mut output, &choices(false), None);
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn a_genuine_io_error_is_not_reported_as_cancelled() {
        // Invalid UTF-8 makes `read_line` itself return `Err`, not `Ok(0)` — this must
        // not be conflated with true EOF (which alone means "the user cancelled").
        let mut input = Cursor::new(vec![0xFF, 0xFE, b'\n']);
        let mut output = Vec::new();
        let result = prompt(&mut input, &mut output, &choices(false), None);
        assert!(matches!(result, Err(PickerError::Io(_))));
    }

    #[test]
    fn to_ccm_error_maps_cancelled_to_picker_cancelled_exit_14() {
        let err = to_ccm_error(PickerError::Cancelled);
        assert_eq!(err.exit_code().as_u8(), 14);
    }

    #[test]
    fn to_ccm_error_maps_io_to_unexpected_exit_1() {
        let io_err = std::io::Error::other("boom");
        let err = to_ccm_error(PickerError::Io(io_err));
        assert_eq!(err.exit_code().as_u8(), 1);
    }

    // ---- interpret_index / prompt_index ----

    #[test]
    fn interpret_index_accepts_in_range_indices() {
        assert_eq!(interpret_index("0", 3), PickerInput::Chosen(0));
        assert_eq!(interpret_index("2", 3), PickerInput::Chosen(2));
    }

    #[test]
    fn interpret_index_rejects_out_of_range_and_garbage() {
        assert_eq!(interpret_index("3", 3), PickerInput::Retry);
        assert_eq!(interpret_index("-1", 3), PickerInput::Retry);
        assert_eq!(interpret_index("", 3), PickerInput::Retry);
        assert_eq!(interpret_index("nope", 3), PickerInput::Retry);
    }

    #[test]
    fn interpret_index_trims_whitespace_and_crlf() {
        assert_eq!(interpret_index(" 1 \r\n", 3), PickerInput::Chosen(1));
    }

    #[test]
    fn prompt_index_selects_a_valid_first_answer() {
        let lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut input = Cursor::new(b"1\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_index(&mut input, &mut output, &lines, None).unwrap();
        assert_eq!(picked, 1);
        let printed = String::from_utf8(output).unwrap();
        assert_eq!(printed, "0) a\n1) b\n2) c\nSelect a tool [0-2]: ");
    }

    #[test]
    fn prompt_index_reprints_menu_and_retries_on_invalid_input() {
        // With no default set, a blank line is just as invalid as "x" or an
        // out-of-range index — all retry.
        let lines = vec!["a".to_string(), "b".to_string()];
        let mut input = Cursor::new(b"x\n\n5\n0\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_index(&mut input, &mut output, &lines, None).unwrap();
        assert_eq!(picked, 0);
        let printed = String::from_utf8(output).unwrap();
        assert_eq!(printed.matches("0) a").count(), 4);
    }

    #[test]
    fn prompt_index_blank_input_selects_the_given_default() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_index(&mut input, &mut output, &lines, Some(1)).unwrap();
        assert_eq!(picked, 1);
    }

    #[test]
    fn prompt_index_blank_input_without_a_default_still_retries() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let mut input = Cursor::new(b"\n0\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_index(&mut input, &mut output, &lines, None).unwrap();
        assert_eq!(picked, 0);
        let printed = String::from_utf8(output).unwrap();
        assert_eq!(printed.matches("0) a").count(), 2);
    }

    #[test]
    fn prompt_index_cancels_on_eof_without_a_valid_selection() {
        let lines = vec!["a".to_string()];
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();
        let result = prompt_index(&mut input, &mut output, &lines, None);
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn prompt_index_eof_still_cancels_even_with_a_default_set() {
        let lines = vec!["a".to_string()];
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();
        let result = prompt_index(&mut input, &mut output, &lines, Some(0));
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn prompt_index_io_error_is_not_cancelled() {
        let lines = vec!["a".to_string()];
        let mut input = Cursor::new(vec![0xFF, 0xFE, b'\n']);
        let mut output = Vec::new();
        let result = prompt_index(&mut input, &mut output, &lines, None);
        assert!(matches!(result, Err(PickerError::Io(_))));
    }

    // ---- interpret_action / prompt_action (the message review prompt) ----

    #[test]
    fn interpret_action_regenerate_is_case_insensitive() {
        assert_eq!(interpret_action(b'r', true), Some(ReviewAction::Regenerate));
        assert_eq!(interpret_action(b'R', true), Some(ReviewAction::Regenerate));
    }

    #[test]
    fn interpret_action_edit_is_case_insensitive() {
        assert_eq!(interpret_action(b'e', true), Some(ReviewAction::Edit));
        assert_eq!(interpret_action(b'E', true), Some(ReviewAction::Edit));
    }

    #[test]
    fn interpret_action_default_is_accept_when_allowed() {
        assert_eq!(interpret_action(b' ', true), Some(ReviewAction::Accept));
        assert_eq!(interpret_action(b'\n', true), Some(ReviewAction::Accept));
        assert_eq!(interpret_action(b'\r', true), Some(ReviewAction::Accept));
    }

    #[test]
    fn interpret_action_default_is_edit_when_accept_not_allowed() {
        // A blank generation has nothing worth accepting, so Space/Enter falls back to
        // Edit instead — and Accept is unreachable no matter what key is pressed.
        assert_eq!(interpret_action(b' ', false), Some(ReviewAction::Edit));
        assert_eq!(interpret_action(b'\n', false), Some(ReviewAction::Edit));
    }

    #[test]
    fn interpret_action_rejects_anything_else() {
        assert_eq!(interpret_action(b'x', true), None);
        assert_eq!(interpret_action(b'2', true), None);
        assert_eq!(interpret_action(0x04, true), None);
    }

    #[test]
    fn prompt_action_selects_regenerate() {
        let mut input = Cursor::new(b"r\n".to_vec());
        let mut output = Vec::new();
        let action = prompt_action(&mut input, &mut output, true, false).unwrap();
        assert_eq!(action, ReviewAction::Regenerate);
    }

    #[test]
    fn prompt_action_selects_edit() {
        let mut input = Cursor::new(b"e\n".to_vec());
        let mut output = Vec::new();
        let action = prompt_action(&mut input, &mut output, true, false).unwrap();
        assert_eq!(action, ReviewAction::Edit);
    }

    #[test]
    fn prompt_action_blank_line_accepts_when_allowed() {
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();
        let action = prompt_action(&mut input, &mut output, true, false).unwrap();
        assert_eq!(action, ReviewAction::Accept);
    }

    #[test]
    fn prompt_action_blank_line_edits_when_accept_is_not_allowed() {
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();
        let action = prompt_action(&mut input, &mut output, false, false).unwrap();
        assert_eq!(action, ReviewAction::Edit);
    }

    #[test]
    fn prompt_action_menu_wording_offers_accept_when_allowed() {
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();
        prompt_action(&mut input, &mut output, true, false).unwrap();
        let printed = String::from_utf8(output).unwrap();
        assert!(printed.contains("[R]egenerate  [E]dit  [Space/Enter] accept: "));
    }

    #[test]
    fn prompt_action_menu_wording_offers_edit_default_when_not_allowed() {
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();
        prompt_action(&mut input, &mut output, false, false).unwrap();
        let printed = String::from_utf8(output).unwrap();
        assert!(printed.contains("[R]egenerate  [Space/Enter] edit: "));
        assert!(!printed.contains("accept"));
    }

    #[test]
    fn prompt_action_reprints_menu_and_retries_on_invalid_input() {
        let mut input = Cursor::new(b"z\n\n".to_vec());
        let mut output = Vec::new();
        let action = prompt_action(&mut input, &mut output, true, false).unwrap();
        assert_eq!(action, ReviewAction::Accept);
        let printed = String::from_utf8(output).unwrap();
        // Printed once for the invalid "z", once more for the winning blank line.
        assert_eq!(printed.matches("[Space/Enter] accept").count(), 2);
    }

    #[test]
    fn prompt_action_line_mode_drains_the_rest_of_the_line() {
        // "r" alone selects Regenerate; the rest of that same line (trailing garbage
        // before the newline) must be discarded so it doesn't leak into whatever reads
        // `input` next — leaving exactly "x\n" behind for that next reader.
        let mut input = Cursor::new(b"rXYZ\nx\n".to_vec());
        let mut output = Vec::new();
        let action = prompt_action(&mut input, &mut output, true, false).unwrap();
        assert_eq!(action, ReviewAction::Regenerate);
        let mut rest = String::new();
        input.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "x\n");
    }

    #[test]
    fn prompt_action_cancels_on_eof_without_a_valid_selection() {
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();
        let result = prompt_action(&mut input, &mut output, true, false);
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn prompt_action_ctrl_d_byte_cancels_even_mid_stream() {
        let mut input = Cursor::new(vec![0x04]);
        let mut output = Vec::new();
        let result = prompt_action(&mut input, &mut output, true, false);
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    /// A `BufRead` double whose every read fails — used to exercise `prompt_action`'s
    /// non-EOF I/O-error path, which real UTF-8-agnostic byte reads (unlike
    /// `read_line`) can't otherwise be made to hit via a plain `Cursor`.
    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("boom"))
        }
    }

    impl BufRead for FailingReader {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            Err(std::io::Error::other("boom"))
        }
        fn consume(&mut self, _amt: usize) {}
    }

    #[test]
    fn prompt_action_io_error_is_not_reported_as_cancelled() {
        let mut input = FailingReader;
        let mut output = Vec::new();
        let result = prompt_action(&mut input, &mut output, true, false);
        assert!(matches!(result, Err(PickerError::Io(_))));
    }
}
