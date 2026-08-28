//! Plain stdin pickers (prd.md, "jj commit commands"; "Selection"; "Message review
//! prompt"): the jj commit-command picker (never more than two choices, a
//! single-keypress prompt like the review prompt), default mode's tool picker (an
//! arbitrary number of `api.yaml` entries, shown only when the choice is ambiguous), and
//! the message review prompt (regenerate/edit/accept, shown after every generation) —
//! all plain stdin prompts, so no fuzzy-finder or TUI crate is warranted (see
//! "Preferences of Dependencies" #3).

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

/// One line of picker input, already interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerInput {
    Chosen(usize),
    Retry,
}

/// Why a picker didn't return a selection.
#[derive(Debug, thiserror::Error)]
pub enum PickerError {
    /// True EOF (or Ctrl-D) without ever making a valid selection — prd.md's "the user
    /// canceled the jj-command picker" (exit 14).
    #[error("aborted: no jj command selected")]
    Cancelled,
    /// A genuine I/O error reading input (e.g. invalid UTF-8 on stdin) — distinct
    /// from EOF and not itself a "the user cancelled" signal, so callers should not
    /// report it the same way.
    #[error("failed to read picker input: {0}")]
    Io(#[source] std::io::Error),
}

/// Interprets one byte of jj commit-command picker input (prd.md "jj commit commands"):
/// `c`/`C`, a space, or Enter (`\n`/`\r`) selects `jj commit` — index 0, the one choice
/// always on offer and so the picker's default; `d`/`D` selects `jj describe` and `s`/`S`
/// selects `jj split`, but each only when that command is the one `choices` actually
/// offers at index 1 (exactly one of describe/split is ever available — see [`choices`]).
/// Any other byte is a retry (`None`).
///
/// Ctrl-D (`0x04`) is deliberately not classified here, same as [`interpret_action`]: it
/// cancels the prompt outright, which is `read_key`'s job, not this classifier's.
#[must_use]
pub fn interpret_commit_key(byte: u8, choices: &[JjCommitCommand; 2]) -> Option<JjCommitCommand> {
    match byte {
        b'c' | b'C' | b' ' | b'\n' | b'\r' => Some(JjCommitCommand::Commit),
        b'd' | b'D' if choices[1] == JjCommitCommand::Describe => Some(JjCommitCommand::Describe),
        b's' | b'S' if choices[1] == JjCommitCommand::Split => Some(JjCommitCommand::Split),
        _ => None,
    }
}

/// The `[K]ey`-style menu label for one jj subcommand, keyed on its first letter — the
/// same letter [`interpret_commit_key`] accepts for it.
fn key_label(cmd: JjCommitCommand) -> &'static str {
    match cmd {
        JjCommitCommand::Commit => "[C]ommit",
        JjCommitCommand::Describe => "[D]escribe",
        JjCommitCommand::Split => "[S]plit",
    }
}

fn print_commit_menu(writer: &mut (impl Write + ?Sized), choices: &[JjCommitCommand; 2]) {
    // `choices[0]` is always `jj commit` (see `choices`), and a space/Enter selects it,
    // hence the fixed `[Space/Enter/C]ommit` prefix; `choices[1]` is the describe-or-split
    // alternative.
    let _ = write!(writer, "[Space/Enter/C]ommit  {}: ", key_label(choices[1]));
    let _ = writer.flush();
}

/// Runs the jj commit-command picker (prd.md "jj commit commands") to a selection:
/// prints `[Space/Enter/C]ommit  [D]escribe: ` (or `[S]plit`, whichever [`choices`]
/// offers at index 1), reads a single key, and repeats on any input
/// [`interpret_commit_key`] doesn't recognize. `jj commit` is always on offer and is the
/// default — a space or Enter selects it — so there is no separate `(default)` marker.
///
/// Raw-vs-line-mode handling and EOF/Ctrl-D cancellation are exactly [`prompt_action`]'s:
/// both read through `read_key`, so `raw` means the same single-keypress-on-a-real-
/// terminal behavior here (see `read_key` and the caller in `commit.rs`).
///
/// # Errors
/// [`PickerError::Cancelled`] on EOF or Ctrl-D before a valid selection;
/// [`PickerError::Io`] if reading a byte fails for a reason other than EOF.
pub fn prompt_commit(
    reader: &mut (impl BufRead + ?Sized),
    writer: &mut (impl Write + ?Sized),
    choices: &[JjCommitCommand; 2],
    raw: bool,
) -> Result<JjCommitCommand, PickerError> {
    loop {
        print_commit_menu(writer, choices);
        let byte = read_key(reader, raw)?;
        if let Some(cmd) = interpret_commit_key(byte, choices) {
            let _ = writeln!(writer);
            return Ok(cmd);
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

/// Strips `config::listing::lines`' trailing `  (default)` marker from a display line,
/// if present. [`prompt_index`]'s own prompt line already states the default index
/// directly (`Enter an index [default N]: `), making the per-entry marker redundant
/// there — even though `--list-tools` and the fzf front-end (`fzf::Fzf`), which have no
/// such prompt line of their own, still show it (see `config::listing::lines`).
fn without_default_marker(line: &str) -> &str {
    line.strip_suffix("  (default)").unwrap_or(line)
}

/// Default mode's tool picker: an arbitrary-length numbered menu. Prints each of `lines`
/// as `N) <line>`, with any trailing `(default)` marker `config::listing::lines` baked
/// in stripped back off (see [`without_default_marker`] — this function's own prompt
/// line states the default index instead), then `Enter an index [default N]: ` (or,
/// when `default` is `None` — nothing is enabled — `Enter an index [0-N]: `) with no
/// trailing newline (flushed so it's visible before `reader` blocks), and reprints the
/// whole menu on any invalid input — except a blank or all-whitespace line, which
/// selects `default` immediately when it's `Some`. True EOF is [`PickerError::Cancelled`]
/// (unaffected by `default`: it signals a closed/non-interactive stdin rather than the
/// user pressing Enter), a non-EOF read failure is [`PickerError::Io`]. `lines` must be
/// non-empty.
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
            let _ = writeln!(writer, "{index}) {}", without_default_marker(line));
        }
        let prompt = match default {
            Some(d) => format!("Enter an index [default {d}]: "),
            None => format!("Enter an index [0-{last}]: "),
        };
        let _ = write!(writer, "{prompt}");
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
/// [`ReviewAction::Regenerate`]; `e`/`E` is always [`ReviewAction::Edit`]; `a`/`A` is
/// [`ReviewAction::Accept`] when `allow_accept` (and a retry otherwise, since a blank
/// generation offers no accept option); a space or Enter (`\n`/`\r`) is the prompt's
/// default action — [`ReviewAction::Accept`] when `allow_accept` (a non-blank generated
/// message), or [`ReviewAction::Edit`] otherwise, since a blank generation has nothing
/// worth accepting (see "Message review prompt"). Any other byte is a retry (`None`).
///
/// Ctrl-D (`0x04`) is deliberately not classified here: unlike every other
/// unrecognized byte, it doesn't mean "keep asking, try again" — it cancels the prompt
/// outright, which is `read_key`'s job, not this pure classifier's.
#[must_use]
pub fn interpret_action(byte: u8, allow_accept: bool) -> Option<ReviewAction> {
    match byte {
        b'r' | b'R' => Some(ReviewAction::Regenerate),
        b'e' | b'E' => Some(ReviewAction::Edit),
        b'a' | b'A' if allow_accept => Some(ReviewAction::Accept),
        b' ' | b'\n' | b'\r' => Some(if allow_accept {
            ReviewAction::Accept
        } else {
            ReviewAction::Edit
        }),
        _ => None,
    }
}

fn print_action_menu(writer: &mut (impl Write + ?Sized), allow_accept: bool) {
    let _ = writeln!(writer, "What to do with the generated message?");
    if allow_accept {
        let _ = write!(
            writer,
            "[R]egenerate    [E]dit    [Space/Enter/A]ccept as is: "
        );
    } else {
        let _ = write!(writer, "[R]egenerate    [Space/Enter] edit: ");
    }
    let _ = writer.flush();
}

/// Reads and discards the remainder of the current line (up to and including the
/// terminating `\n`, or EOF), one byte at a time. Only used in line mode (`raw ==
/// false` in [`read_key`]): since a non-terminal stdin delivers a whole typed line
/// at once regardless of which single byte we act on, this keeps every attempt —
/// whether it selected a valid action or was a retry — consuming exactly one line, the
/// same granularity [`prompt_index`] reads at via `read_line`. Without this,
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

/// Reads one key for a single-keypress prompt — [`prompt_action`] and [`prompt_commit`]
/// share this.
///
/// When `raw` is `true`, process stdin is put into raw terminal mode
/// ([`term::RawGuard`]) for the read, so a single keypress is returned with no Enter
/// needed — callers pass `true` only when `Environment::stdin_is_terminal` reports a real
/// terminal, and `reader` is that same terminal's stdin then. When raw mode isn't
/// actually engaged for the read — either `raw` is `false` (stdin isn't a real terminal —
/// every integration test, and any piped caller), or `raw` is `true` but
/// [`term::RawGuard::enable`] itself fails despite stdin reporting as a terminal — only
/// the first byte of the line is inspected, and [`drain_rest_of_line`] discards the
/// remainder — up to and including the terminating `\n`, or EOF — so the next read of
/// `reader` (a retry of the same prompt, or the next prompt — e.g. the jj commit-command
/// picker right after the review prompt) always starts at a clean line boundary; skipping
/// this drain is only correct when raw mode genuinely suppressed canonical line
/// buffering.
///
/// # Errors
/// [`PickerError::Cancelled`] on EOF (zero bytes read) or a literal Ctrl-D byte
/// (`0x04`), regardless of `raw` — the same "the user didn't finish choosing" signal
/// every picker treats as a cancel; [`PickerError::Io`] if the read fails for a reason
/// other than EOF.
fn read_key(reader: &mut (impl BufRead + ?Sized), raw: bool) -> Result<u8, PickerError> {
    let mut buf = [0u8; 1];
    // Whether raw mode was actually engaged, not merely requested: if `raw` is true but
    // `RawGuard::enable` itself fails (e.g. `tcgetattr`/`tcsetattr` erroring despite stdin
    // reporting as a terminal), the terminal stays in canonical line-buffered mode, and
    // skipping the drain below on `raw` alone would leave a trailing `\n` (or more)
    // unconsumed, leaking into the next read.
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

    Ok(byte)
}

/// Runs the message review prompt (prd.md "Message review prompt") to a selection:
/// prints a `What to do with the generated message?` heading followed by
/// `[R]egenerate    [E]dit    [Space/Enter/A]ccept as is: ` (or, when `allow_accept` is
/// `false` — the generated message is blank — `[R]egenerate    [Space/Enter] edit: `),
/// reads a single key via `read_key`, and repeats (heading included) on any input
/// [`interpret_action`] doesn't recognize. See `read_key` for the raw-vs-line-mode and
/// EOF/Ctrl-D behavior. A newline is written after a valid selection, since raw mode
/// echoes nothing back to the terminal on its own.
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
        let byte = read_key(reader, raw)?;
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
    fn interpret_commit_key_c_space_enter_all_select_commit() {
        let unscoped = choices(false);
        for &byte in b"cC \n\r" {
            assert_eq!(
                interpret_commit_key(byte, &unscoped),
                Some(JjCommitCommand::Commit)
            );
        }
    }

    #[test]
    fn interpret_commit_key_d_selects_describe_only_when_offered() {
        assert_eq!(
            interpret_commit_key(b'd', &choices(false)),
            Some(JjCommitCommand::Describe)
        );
        assert_eq!(
            interpret_commit_key(b'D', &choices(false)),
            Some(JjCommitCommand::Describe)
        );
        // A scoped run offers split, not describe — `d` is then just a retry.
        assert_eq!(interpret_commit_key(b'd', &choices(true)), None);
    }

    #[test]
    fn interpret_commit_key_s_selects_split_only_when_offered() {
        assert_eq!(
            interpret_commit_key(b's', &choices(true)),
            Some(JjCommitCommand::Split)
        );
        assert_eq!(
            interpret_commit_key(b'S', &choices(true)),
            Some(JjCommitCommand::Split)
        );
        // An unscoped run offers describe, not split — `s` is then just a retry.
        assert_eq!(interpret_commit_key(b's', &choices(false)), None);
    }

    #[test]
    fn interpret_commit_key_rejects_anything_else() {
        // The old numeric `0`/`1` input is no longer accepted.
        assert_eq!(interpret_commit_key(b'0', &choices(false)), None);
        assert_eq!(interpret_commit_key(b'1', &choices(false)), None);
        assert_eq!(interpret_commit_key(b'x', &choices(false)), None);
        assert_eq!(interpret_commit_key(0x04, &choices(false)), None);
    }

    #[test]
    fn prompt_commit_selects_commit_on_c() {
        let mut input = Cursor::new(b"c\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_commit(&mut input, &mut output, &choices(false), false).unwrap();
        assert_eq!(picked, JjCommitCommand::Commit);
    }

    #[test]
    fn prompt_commit_blank_line_selects_commit_as_the_default() {
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_commit(&mut input, &mut output, &choices(true), false).unwrap();
        assert_eq!(picked, JjCommitCommand::Commit);
    }

    #[test]
    fn prompt_commit_space_selects_commit() {
        let mut input = Cursor::new(b" \n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_commit(&mut input, &mut output, &choices(false), false).unwrap();
        assert_eq!(picked, JjCommitCommand::Commit);
    }

    #[test]
    fn prompt_commit_eof_cancels_rather_than_selecting_the_default() {
        // A blank *line* selects the default (jj commit), but EOF — stdin closed with
        // nothing typed — is a distinct signal and must still cancel with exit 14, never
        // fall back to the default (prd.md "jj commit commands").
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();
        let result = prompt_commit(&mut input, &mut output, &choices(false), false);
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn prompt_commit_selects_describe_when_unscoped() {
        let mut input = Cursor::new(b"d\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_commit(&mut input, &mut output, &choices(false), false).unwrap();
        assert_eq!(picked, JjCommitCommand::Describe);
    }

    #[test]
    fn prompt_commit_selects_split_when_scoped() {
        let mut input = Cursor::new(b"s\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_commit(&mut input, &mut output, &choices(true), false).unwrap();
        assert_eq!(picked, JjCommitCommand::Split);
    }

    #[test]
    fn prompt_commit_menu_wording_unscoped_offers_describe() {
        let mut input = Cursor::new(b"c\n".to_vec());
        let mut output = Vec::new();
        prompt_commit(&mut input, &mut output, &choices(false), false).unwrap();
        let printed = String::from_utf8(output).unwrap();
        assert!(printed.contains("[Space/Enter/C]ommit  [D]escribe: "));
    }

    #[test]
    fn prompt_commit_menu_wording_scoped_offers_split_not_describe() {
        let mut input = Cursor::new(b"c\n".to_vec());
        let mut output = Vec::new();
        prompt_commit(&mut input, &mut output, &choices(true), false).unwrap();
        let printed = String::from_utf8(output).unwrap();
        assert!(printed.contains("[Space/Enter/C]ommit  [S]plit: "));
        assert!(!printed.contains("[D]escribe"));
    }

    #[test]
    fn prompt_commit_reprints_menu_and_retries_on_invalid_input() {
        // "x" and "0" (the old numeric input, now invalid) both retry; the blank line
        // then selects the default — three menu prints in all.
        let mut input = Cursor::new(b"x\n0\n\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_commit(&mut input, &mut output, &choices(false), false).unwrap();
        assert_eq!(picked, JjCommitCommand::Commit);
        let printed = String::from_utf8(output).unwrap();
        assert_eq!(printed.matches("[Space/Enter/C]ommit").count(), 3);
    }

    #[test]
    fn prompt_commit_ctrl_d_byte_cancels() {
        let mut input = Cursor::new(vec![0x04]);
        let mut output = Vec::new();
        let result = prompt_commit(&mut input, &mut output, &choices(false), false);
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn prompt_commit_cancels_on_eof_after_some_invalid_attempts() {
        let mut input = Cursor::new(b"garbage\n".to_vec());
        let mut output = Vec::new();
        let result = prompt_commit(&mut input, &mut output, &choices(false), false);
        assert!(matches!(result, Err(PickerError::Cancelled)));
    }

    #[test]
    fn prompt_commit_line_mode_drains_the_rest_of_the_line() {
        // "d" alone selects Describe; the trailing garbage before the newline must be
        // discarded so it doesn't leak into whatever reads `input` next.
        let mut input = Cursor::new(b"dXYZ\nx\n".to_vec());
        let mut output = Vec::new();
        let picked = prompt_commit(&mut input, &mut output, &choices(false), false).unwrap();
        assert_eq!(picked, JjCommitCommand::Describe);
        let mut rest = String::new();
        input.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "x\n");
    }

    #[test]
    fn prompt_commit_io_error_is_not_reported_as_cancelled() {
        let mut input = FailingReader;
        let mut output = Vec::new();
        let result = prompt_commit(&mut input, &mut output, &choices(false), false);
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
        assert_eq!(printed, "0) a\n1) b\n2) c\nEnter an index [0-2]: ");
    }

    #[test]
    fn prompt_index_states_the_default_in_the_prompt_and_strips_its_marker_from_the_line() {
        let lines = vec![
            "a  agent_cli  [enabled]".to_string(),
            "b  agent_cli  [enabled]  (default)".to_string(),
        ];
        let mut input = Cursor::new(b"0\n".to_vec());
        let mut output = Vec::new();
        prompt_index(&mut input, &mut output, &lines, Some(1)).unwrap();
        let printed = String::from_utf8(output).unwrap();
        assert!(!printed.contains("(default)"));
        assert!(printed.contains("Enter an index [default 1]: "));
    }

    #[test]
    fn without_default_marker_strips_only_a_genuine_trailing_marker() {
        assert_eq!(
            without_default_marker("a  [enabled]  (default)"),
            "a  [enabled]"
        );
        assert_eq!(without_default_marker("a  [enabled]"), "a  [enabled]");
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
    fn interpret_action_accept_key_is_case_insensitive_when_allowed() {
        assert_eq!(interpret_action(b'a', true), Some(ReviewAction::Accept));
        assert_eq!(interpret_action(b'A', true), Some(ReviewAction::Accept));
    }

    #[test]
    fn interpret_action_default_is_edit_when_accept_not_allowed() {
        // A blank generation has nothing worth accepting, so Space/Enter falls back to
        // Edit instead — and Accept is unreachable no matter what key is pressed, the
        // explicit `a`/`A` accept key included (it's a retry then).
        assert_eq!(interpret_action(b' ', false), Some(ReviewAction::Edit));
        assert_eq!(interpret_action(b'\n', false), Some(ReviewAction::Edit));
        assert_eq!(interpret_action(b'a', false), None);
        assert_eq!(interpret_action(b'A', false), None);
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
    fn prompt_action_selects_accept_on_a_key() {
        let mut input = Cursor::new(b"a\n".to_vec());
        let mut output = Vec::new();
        let action = prompt_action(&mut input, &mut output, true, false).unwrap();
        assert_eq!(action, ReviewAction::Accept);
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
        assert!(printed.contains("What to do with the generated message?"));
        assert!(printed.contains("[R]egenerate    [E]dit    [Space/Enter/A]ccept as is: "));
    }

    #[test]
    fn prompt_action_menu_wording_offers_edit_default_when_not_allowed() {
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();
        prompt_action(&mut input, &mut output, false, false).unwrap();
        let printed = String::from_utf8(output).unwrap();
        assert!(printed.contains("What to do with the generated message?"));
        assert!(printed.contains("[R]egenerate    [Space/Enter] edit: "));
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
        assert_eq!(printed.matches("[Space/Enter/A]ccept").count(), 2);
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

    /// A `BufRead` double whose every read fails — used to exercise the non-EOF
    /// I/O-error path of `prompt_action`/`prompt_commit` (via `read_key`), which real
    /// UTF-8-agnostic byte reads (unlike `read_line`) can't otherwise be made to hit via
    /// a plain `Cursor`.
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
