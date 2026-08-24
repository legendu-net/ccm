//! The jj commit-command picker (prd.md, "jj commit commands"): a plain numbered
//! stdin prompt, never more than two choices, so no fuzzy-finder crate is warranted
//! (see "Preferences of Dependencies" #3).

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
}
