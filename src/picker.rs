//! Plain numbered stdin pickers (prd.md, "jj commit commands"; "Selection"): the jj
//! commit-command picker (never more than two choices) and the `--interactive` tool
//! picker (an arbitrary number of `api.yaml` entries) — both plain stdin prompts, so no
//! fuzzy-finder crate is warranted (see "Preferences of Dependencies" #3).

use crate::error::{CcmError, PickerCancelled};
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
/// commit` / `1) jj describe|split`, reads a line from `reader`, and repeats on any
/// invalid (non-`"0"`/`"1"`) input.
///
/// # Errors
/// [`PickerError::Cancelled`] on EOF without ever selecting a valid index;
/// [`PickerError::Io`] if reading a line fails for a reason other than EOF.
pub fn prompt(
    reader: &mut (impl BufRead + ?Sized),
    writer: &mut (impl Write + ?Sized),
    choices: &[JjCommitCommand; 2],
) -> Result<JjCommitCommand, PickerError> {
    loop {
        let _ = writeln!(writer, "0) {}", label(choices[0]));
        let _ = writeln!(writer, "1) {}", label(choices[1]));

        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).map_err(PickerError::Io)?;
        if bytes_read == 0 {
            return Err(PickerError::Cancelled);
        }

        match interpret(&line) {
            PickerInput::Chosen(index) => return Ok(choices[index]),
            PickerInput::Retry => {}
        }
    }
}

/// Maps a [`PickerError`] to its documented exit code: true EOF without a selection is
/// [`PickerCancelled`] (exit 14); any other error (e.g. invalid UTF-8 on stdin) is the
/// generic exit-1 catch-all. Shared by every picker caller (the jj commit-command picker
/// in `commit.rs`, the `--interactive` tool picker in `pipeline.rs`) so the two variants
/// are never mapped inconsistently.
#[must_use]
pub fn to_ccm_error(err: PickerError) -> CcmError {
    if matches!(err, PickerError::Cancelled) {
        CcmError::from(PickerCancelled)
    } else {
        CcmError::Unexpected(err.to_string())
    }
}

/// Interprets one line of `--interactive` tool-picker input: trimmed, then accepted as a
/// selection only if it parses as a `usize` strictly less than `count`; anything else —
/// including a blank line, a negative number, or an out-of-range index — is a retry.
#[must_use]
pub fn interpret_index(line: &str, count: usize) -> PickerInput {
    match line.trim().parse::<usize>() {
        Ok(index) if index < count => PickerInput::Chosen(index),
        _ => PickerInput::Retry,
    }
}

/// The `--interactive` tool picker: an arbitrary-length numbered-menu variant of
/// [`prompt`]. Prints each of `lines` as `N) <line>`, then a `Select a tool [0-N]: `
/// prompt with no trailing newline (flushed so it's visible before `reader` blocks), and
/// reprints the whole menu on any invalid input — same EOF-is-[`PickerError::Cancelled`]
/// / IO-error-is-[`PickerError::Io`] split as [`prompt`]. `lines` must be non-empty.
///
/// # Errors
/// [`PickerError::Cancelled`] on EOF without ever selecting a valid index;
/// [`PickerError::Io`] if reading a line fails for a reason other than EOF.
pub fn prompt_index(
    reader: &mut (impl BufRead + ?Sized),
    writer: &mut (impl Write + ?Sized),
    lines: &[String],
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
            PickerInput::Retry => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
        let picked = prompt(&mut input, &mut output, &choices(false)).unwrap();
        assert_eq!(picked, JjCommitCommand::Commit);
        let printed = String::from_utf8(output).unwrap();
        assert_eq!(printed, "0) jj commit\n1) jj describe\n");
    }

    #[test]
    fn prompt_selects_index_one() {
        let mut input = Cursor::new(b"1\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt(&mut input, &mut output, &choices(true)).unwrap();
        assert_eq!(picked, JjCommitCommand::Split);
    }

    #[test]
    fn prompt_reprints_choices_and_retries_on_invalid_input() {
        let mut input = Cursor::new(b"x\n\n0\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt(&mut input, &mut output, &choices(false)).unwrap();
        assert_eq!(picked, JjCommitCommand::Commit);
        let printed = String::from_utf8(output).unwrap();
        // Printed once per attempt: invalid "x", blank, then the winning "0" — three
        // reprints of the two-line menu.
        assert_eq!(printed.matches("0) jj commit").count(), 3);
    }

    #[test]
    fn prompt_cancels_on_eof_without_a_valid_selection() {
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();
        let result = prompt(&mut input, &mut output, &choices(false));
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn prompt_cancels_on_eof_after_some_invalid_attempts() {
        let mut input = Cursor::new(b"garbage\n".to_vec());
        let mut output = Vec::new();
        let result = prompt(&mut input, &mut output, &choices(false));
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn a_genuine_io_error_is_not_reported_as_cancelled() {
        // Invalid UTF-8 makes `read_line` itself return `Err`, not `Ok(0)` — this must
        // not be conflated with true EOF (which alone means "the user cancelled").
        let mut input = Cursor::new(vec![0xFF, 0xFE, b'\n']);
        let mut output = Vec::new();
        let result = prompt(&mut input, &mut output, &choices(false));
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
        let picked = prompt_index(&mut input, &mut output, &lines).unwrap();
        assert_eq!(picked, 1);
        let printed = String::from_utf8(output).unwrap();
        assert_eq!(printed, "0) a\n1) b\n2) c\nSelect a tool [0-2]: ");
    }

    #[test]
    fn prompt_index_reprints_menu_and_retries_on_invalid_input() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let mut input = Cursor::new(b"x\n\n5\n0\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_index(&mut input, &mut output, &lines).unwrap();
        assert_eq!(picked, 0);
        let printed = String::from_utf8(output).unwrap();
        assert_eq!(printed.matches("0) a").count(), 4);
    }

    #[test]
    fn prompt_index_cancels_on_eof_without_a_valid_selection() {
        let lines = vec!["a".to_string()];
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();
        let result = prompt_index(&mut input, &mut output, &lines);
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn prompt_index_io_error_is_not_cancelled() {
        let lines = vec!["a".to_string()];
        let mut input = Cursor::new(vec![0xFF, 0xFE, b'\n']);
        let mut output = Vec::new();
        let result = prompt_index(&mut input, &mut output, &lines);
        assert!(matches!(result, Err(PickerError::Io(_))));
    }
}
