//! An optional, runtime-detected [`fzf`](https://github.com/junegunn/fzf) subprocess as
//! the tool picker's fuzzy-search front-end (prd.md "Selection", "Preferences of
//! Dependencies" item 8) — shelled out to exactly like `git`/`jj` themselves, never
//! embedded as a library. `fzf` reads candidates from the piped stdin `ccm` writes it
//! and reads its selection back from its stdout — the standard `command | fzf` idiom,
//! just invoked via [`Command`] instead of a shell pipe. Its own interactive rendering
//! and keyboard input go through `/dev/tty` directly. If `fzf` isn't on `$PATH`, has no
//! controlling terminal available, or fails any other way, the caller (`pipeline.rs`,
//! stage 5) falls back to `picker::prompt_index` — this module adds no new
//! `Cargo.toml` dependency and no way for a run to end up worse off than without `fzf`
//! installed. Setting `CCM_FUZZY=0` (see [`disabled_by_env`]) makes that same caller
//! skip the `fzf` front-end outright — the escape hatch for a terminal that mishandles
//! `fzf`'s inline TUI (prd.md "Selection").
//!
//! Deliberately does **not** go through `vcs::exec::run_capture` (used for git/jj/
//! agent_cli): that helper puts the child in its own new process group
//! (`process_group(0)`) so a `timeout`-triggered kill can signal a whole subtree — but
//! a process in a background process group that tries to read its controlling terminal
//! gets `SIGTTIN`, whose default disposition is to *stop* it, not fail it. `fzf`
//! reading `/dev/tty` from a `process_group(0)`-isolated child hangs forever this way
//! (confirmed empirically: state `T`, never reaped, since there's no timeout to rescue
//! it either). `fzf` is spawned here as a perfectly ordinary child instead — no process-
//! group manipulation at all, inheriting `ccm`'s own group exactly like `$EDITOR`
//! already does in `editor/mod.rs` — so it's part of the terminal's foreground group
//! from the start and can read `/dev/tty` normally.

use crate::vcs::exec::{Captured, ExecError, owned_utf8_lossy};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

/// How a [`Fzf::select`] attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FzfOutcome {
    /// The user picked the candidate at this index into the `lines` passed to
    /// [`Fzf::select`].
    Selected(usize),
    /// The user explicitly aborted (Esc/Ctrl-C — `fzf` exit 130) or confirmed with
    /// nothing matched (`fzf` exit 1). The fuzzy-picker equivalent of the numbered
    /// prompt's EOF; the caller maps this to exit 14.
    Cancelled,
    /// `fzf` couldn't run at all (not found on `$PATH`, no controlling terminal, killed
    /// by a signal), or exited some other way not recognized as a clean selection or an
    /// abort. The caller falls back to the numbered prompt; the string is a one-line
    /// reason to log on stderr before doing so.
    Unavailable(String),
}

/// How a [`Fzf::select_files`] attempt ended — the multi-select analogue of
/// [`FzfOutcome`] for the interactive diff-scope picker (prd.md "Diff scope
/// resolution"). Indices are into the `lines` passed to `select_files`, in the same
/// (first-seen, deduplicated) order `fileselect.rs` built them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FzfMultiOutcome {
    /// The user marked and confirmed one or more candidates. Never empty — a
    /// confirm with nothing marked is indistinguishable from "nothing matched" and is
    /// [`FzfMultiOutcome::Cancelled`] instead (see [`interpret_multi`]).
    Selected(Vec<usize>),
    /// Esc/Ctrl-C (`fzf` exit 130), or Enter with nothing marked and nothing matched
    /// (`fzf` exit 1, or exit 0 with empty stdout).
    Cancelled,
    /// Same set of reasons as [`FzfOutcome::Unavailable`].
    Unavailable(String),
}

/// Where `pipeline::run_with`'s stage 5 gets the tool picker's fuzzy front-end from —
/// abstracted so that gating logic (call `fzf` only when stdin is a terminal, map each
/// [`FzfOutcome`] to the right control flow, fall back to the numbered prompt on
/// [`FzfOutcome::Unavailable`]) is unit-testable against a substitute, without spawning
/// a real subprocess. What this seam does *not* cover — `fzf`'s own keystroke handling
/// and rendering — can't be tested this way regardless of the seam; see this module's
/// own doc comment and the plan's manual verification steps for that.
pub trait Fzf {
    fn select(&self, cwd: &Path, lines: &[String]) -> FzfOutcome;

    /// The multi-select front-end for the interactive diff-scope picker
    /// (`fileselect.rs`, prd.md "Diff scope resolution") — same shelling-out
    /// mechanism as [`Fzf::select`], but allows marking any number of candidates
    /// (`--multi`) and previews each highlighted file's diff.
    fn select_files(&self, cwd: &Path, lines: &[String]) -> FzfMultiOutcome;
}

/// Whether `$CCM_FUZZY` is set to exactly `0` — the escape hatch (prd.md "Selection")
/// for forcing the numbered stdin prompt even on a real terminal, e.g. one that
/// mishandles `fzf`'s inline TUI. Pass `env.var("CCM_FUZZY").as_deref()`. Any other
/// value, including unset or empty, leaves the `fzf` front-end enabled.
#[must_use]
pub fn disabled_by_env(ccm_fuzzy: Option<&str>) -> bool {
    ccm_fuzzy == Some("0")
}

/// The real, production front-end: shells out via [`Fzf::select`].
pub struct RealFzf;

impl Fzf for RealFzf {
    fn select(&self, cwd: &Path, lines: &[String]) -> FzfOutcome {
        select(cwd, lines)
    }

    fn select_files(&self, cwd: &Path, lines: &[String]) -> FzfMultiOutcome {
        select_files(cwd, lines)
    }
}

/// Runs `fzf` over `lines` (in order, one candidate per line — the same rendering
/// `config::listing::lines` and `--list-tools` use) and reports what happened. Never
/// panics and never blocks longer than the user takes to respond — there's no timeout,
/// matching every other interactive picker in this codebase (a human thinking is not a
/// hung process).
#[must_use]
fn select(cwd: &Path, lines: &[String]) -> FzfOutcome {
    let stdin_bytes = format!("{}\n", lines.join("\n")).into_bytes();
    interpret(spawn(cwd, &build_args(), stdin_bytes), lines)
}

fn build_args() -> Vec<String> {
    vec![
        "--prompt=Select a tool> ".to_string(),
        "--height=40%".to_string(),
        "--layout=reverse".to_string(),
        "--no-multi".to_string(),
    ]
}

/// Runs `fzf` over `lines` (`"<status>  <path>"` rows built from parsed
/// `jj diff --summary` entries — see `fileselect.rs`) in multi-select mode (`--multi`,
/// Tab to mark) with a live per-file diff preview, and reports what happened.
#[must_use]
fn select_files(cwd: &Path, lines: &[String]) -> FzfMultiOutcome {
    let stdin_bytes = format!("{}\n", lines.join("\n")).into_bytes();
    interpret_multi(spawn(cwd, &build_files_args(), stdin_bytes), lines)
}

/// Args for the file picker's `fzf` invocation. `--preview` runs
/// `jj --no-pager diff --color=always -- {2..}` (`--color=always` rather than the
/// `--color=never` "Diff generation" uses elsewhere: this output goes straight to
/// fzf's own preview pane, a real terminal, not somewhere `ccm` parses it). `{2..}` is
/// fzf's own placeholder for "every field from the 2nd column onward" of the
/// *highlighted* line (field 1 is the leading status letter — see `fileselect.rs`'s
/// candidate-line format), substituted
/// and shell-quoted by fzf itself before it's handed to `$SHELL -c`. This is the one
/// place `ccm` puts a value into a shell command line rather than a `Command::arg()`
/// — unavoidable, since `--preview` is fzf's only mechanism for it — but the command
/// text itself is a fixed literal we control; the only variable part is fzf's own
/// quoted substitution of the currently-highlighted candidate's already-validated
/// path (never arbitrary user input).
fn build_files_args() -> Vec<String> {
    vec![
        "--prompt=Select files> ".to_string(),
        "--multi".to_string(),
        // Taller than the tool picker's plain `--height=40%` (`build_args`, no
        // preview to make room for): a diff needs real vertical space to be
        // legible, not just the list of candidates.
        "--height=90%".to_string(),
        "--layout=reverse".to_string(),
        "--header=Tab to mark, Enter to confirm".to_string(),
        "--preview=jj --no-pager diff --color=always -- {2..}".to_string(),
        "--preview-window=right,70%,wrap".to_string(),
    ]
}

/// Spawns `fzf args...` in `cwd`, feeds it `stdin_bytes` (the candidate list), and
/// captures its exit status/stdout/stderr — deliberately not via `vcs::exec::run_capture`
/// (see this module's doc comment for why). `stdin_bytes` is written on its own thread
/// while the main thread waits for output via [`std::process::Child::wait_with_output`]
/// (which itself reads stdout/stderr concurrently internally), so this can't deadlock
/// on a full pipe buffer regardless of candidate-list size — the same hazard
/// `vcs::exec`'s own module doc describes for large diffs, just avoided by a smaller,
/// dedicated mechanism here rather than reusing that one.
fn spawn(cwd: &Path, args: &[String], stdin_bytes: Vec<u8>) -> Result<Captured, ExecError> {
    let mut child = Command::new("fzf")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ExecError::Spawn)?;

    // Always `Some` — just requested `Stdio::piped()` above.
    let mut child_stdin = child.stdin.take().expect("child stdin was piped");
    let writer = thread::spawn(move || {
        let _ = child_stdin.write_all(&stdin_bytes);
        // `child_stdin` drops here, closing the pipe (EOF) for `fzf` to see.
    });

    let output = child.wait_with_output().map_err(ExecError::Wait)?;
    // Best-effort: a write error (e.g. fzf exited early, closing its read end) is
    // already reflected in `output`; nothing more useful to do with a join failure.
    let _ = writer.join();

    Ok(Captured {
        success: output.status.success(),
        code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// Pure classifier — testable directly with hand-built [`Captured`]/[`ExecError`]
/// values, no subprocess needed. `fzf`'s exit codes (man fzf, "EXIT STATUS") are 0
/// (selected), 1 (no match), 2 (error, e.g. no controlling terminal — verified: piped,
/// non-terminal stdin/stdout/stderr with no `/dev/tty` available produces exit 2
/// immediately, no hang), 130 (Ctrl-C/Esc). Anything else, or the process ending
/// without an exit code at all (killed by a signal), degrades to [`FzfOutcome::Unavailable`]
/// rather than guessing.
fn interpret(result: Result<Captured, ExecError>, lines: &[String]) -> FzfOutcome {
    let captured = match result {
        Ok(c) => c,
        Err(err) => return FzfOutcome::Unavailable(err.to_string()),
    };
    match captured.code {
        Some(0) => {
            let selected = owned_utf8_lossy(captured.stdout);
            let selected = selected.trim_end_matches('\n');
            match lines.iter().position(|line| line.as_str() == selected) {
                Some(index) => FzfOutcome::Selected(index),
                None => FzfOutcome::Unavailable(format!(
                    "fzf returned a line that doesn't match any candidate: {selected:?}"
                )),
            }
        }
        Some(1 | 130) => FzfOutcome::Cancelled,
        Some(code) => FzfOutcome::Unavailable(format!("fzf exited with status {code}")),
        None => FzfOutcome::Unavailable("fzf was terminated by a signal".to_string()),
    }
}

/// The [`interpret`] of [`select_files`]: same exit-code table, but exit 0 carries one
/// line of output per marked candidate (`--multi`) rather than exactly one. Every
/// returned line must match a candidate — one that doesn't degrades the whole result
/// to [`FzfMultiOutcome::Unavailable`], same as [`interpret`]. Exit 0 with no lines at
/// all (confirming with nothing marked and nothing matched the filter) is
/// [`FzfMultiOutcome::Cancelled`], not an empty `Selected(vec![])` — there is nothing
/// useful the caller could do with a scope of zero files that isn't better expressed
/// as a cancellation.
fn interpret_multi(result: Result<Captured, ExecError>, lines: &[String]) -> FzfMultiOutcome {
    let captured = match result {
        Ok(c) => c,
        Err(err) => return FzfMultiOutcome::Unavailable(err.to_string()),
    };
    match captured.code {
        Some(0) => {
            let selected = owned_utf8_lossy(captured.stdout);
            let selected = selected.trim_end_matches('\n');
            if selected.is_empty() {
                return FzfMultiOutcome::Cancelled;
            }
            let mut indices = Vec::new();
            for line in selected.split('\n') {
                match lines
                    .iter()
                    .position(|candidate| candidate.as_str() == line)
                {
                    Some(index) => indices.push(index),
                    None => {
                        return FzfMultiOutcome::Unavailable(format!(
                            "fzf returned a line that doesn't match any candidate: {line:?}"
                        ));
                    }
                }
            }
            FzfMultiOutcome::Selected(indices)
        }
        Some(1 | 130) => FzfMultiOutcome::Cancelled,
        Some(code) => FzfMultiOutcome::Unavailable(format!("fzf exited with status {code}")),
        None => FzfMultiOutcome::Unavailable("fzf was terminated by a signal".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines() -> Vec<String> {
        vec![
            "a  agent_cli  [enabled]  (default)".to_string(),
            "b  agent_cli  [disabled]".to_string(),
        ]
    }

    fn captured(code: i32, stdout: &str) -> Result<Captured, ExecError> {
        Ok(Captured {
            success: code == 0,
            code: Some(code),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    #[test]
    fn ccm_fuzzy_zero_disables_the_fzf_front_end() {
        assert!(disabled_by_env(Some("0")));
    }

    #[test]
    fn ccm_fuzzy_unset_or_any_other_value_leaves_it_enabled() {
        assert!(!disabled_by_env(None));
        assert!(!disabled_by_env(Some("")));
        assert!(!disabled_by_env(Some("1")));
        assert!(!disabled_by_env(Some("false")));
        assert!(!disabled_by_env(Some("00")));
    }

    #[test]
    fn exit_0_with_a_matching_line_selects_its_index() {
        let result = captured(0, "b  agent_cli  [disabled]\n");
        assert_eq!(interpret(result, &lines()), FzfOutcome::Selected(1));
    }

    #[test]
    fn exit_0_with_the_first_line_selects_index_0() {
        let result = captured(0, "a  agent_cli  [enabled]  (default)\n");
        assert_eq!(interpret(result, &lines()), FzfOutcome::Selected(0));
    }

    #[test]
    fn exit_0_with_a_line_matching_nothing_is_unavailable() {
        // Shouldn't happen in practice (fzf can only return what it was given), but
        // must degrade rather than silently misselect if it somehow did.
        let result = captured(0, "not a real candidate\n");
        assert!(matches!(
            interpret(result, &lines()),
            FzfOutcome::Unavailable(_)
        ));
    }

    #[test]
    fn exit_1_no_match_is_cancelled() {
        let result = captured(1, "");
        assert_eq!(interpret(result, &lines()), FzfOutcome::Cancelled);
    }

    #[test]
    fn exit_130_ctrl_c_or_esc_is_cancelled() {
        let result = captured(130, "");
        assert_eq!(interpret(result, &lines()), FzfOutcome::Cancelled);
    }

    #[test]
    fn exit_2_error_is_unavailable() {
        let result = captured(2, "");
        assert!(matches!(
            interpret(result, &lines()),
            FzfOutcome::Unavailable(_)
        ));
    }

    #[test]
    fn an_unrecognized_exit_code_is_unavailable() {
        let result = captured(7, "");
        assert!(matches!(
            interpret(result, &lines()),
            FzfOutcome::Unavailable(_)
        ));
    }

    #[test]
    fn killed_by_a_signal_is_unavailable() {
        let result = Ok(Captured {
            success: false,
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
        assert!(matches!(
            interpret(result, &lines()),
            FzfOutcome::Unavailable(_)
        ));
    }

    #[test]
    fn spawn_failure_is_unavailable() {
        let result: Result<Captured, ExecError> = Err(ExecError::Spawn(std::io::Error::other(
            "no such file or directory",
        )));
        assert!(matches!(
            interpret(result, &lines()),
            FzfOutcome::Unavailable(_)
        ));
    }

    #[test]
    fn select_against_the_real_subprocess_degrades_safely_with_no_terminal() {
        // Exercises the real `select` -> `spawn` -> `Command::new("fzf")` path for
        // real, unlike every other test in this module. Only meaningful without a
        // controlling terminal (CI, a headless runner, `setsid cargo test`): there
        // this must land on `Unavailable` either way (ExecError::Spawn if `fzf` isn't
        // found; fzf's own exit 2 "no /dev/tty" if it is — see this module's doc
        // comment for both). `fzf` opens `/dev/tty` directly regardless of the piped
        // stdio a test gets, so when that path is openable it would instead render its
        // UI and block for a keypress — skip then rather than hijack the terminal.
        // What this can't prove — the actual interactive selection UI, and that
        // removing process-group isolation fixes the SIGTTIN hang — needs a real
        // terminal; see the plan's manual verification steps for that.
        if std::fs::File::open("/dev/tty").is_ok() {
            return;
        }
        let outcome = select(&std::env::current_dir().unwrap(), &lines());
        assert!(matches!(outcome, FzfOutcome::Unavailable(_)));
    }

    // ---- select_files / build_files_args / interpret_multi ----

    fn file_lines() -> Vec<String> {
        vec![
            "M  src/diff.rs".to_string(),
            "A  src/fileselect.rs".to_string(),
            "D  src/old.rs".to_string(),
        ]
    }

    #[test]
    fn build_files_args_requests_multi_select_and_a_diff_preview() {
        let args = build_files_args();
        assert!(args.contains(&"--multi".to_string()));
        assert!(args.contains(&"--preview=jj --no-pager diff --color=always -- {2..}".to_string()));
    }

    #[test]
    fn multi_exit_0_with_one_matching_line_selects_its_index() {
        let result = captured(0, "A  src/fileselect.rs\n");
        assert_eq!(
            interpret_multi(result, &file_lines()),
            FzfMultiOutcome::Selected(vec![1])
        );
    }

    #[test]
    fn multi_exit_0_with_several_lines_selects_every_index_in_candidate_order() {
        let result = captured(0, "D  src/old.rs\nM  src/diff.rs\n");
        assert_eq!(
            interpret_multi(result, &file_lines()),
            FzfMultiOutcome::Selected(vec![2, 0])
        );
    }

    #[test]
    fn multi_exit_0_with_empty_stdout_is_cancelled() {
        // Confirming with nothing marked and nothing matched the filter — distinct
        // from `Selected(vec![])`, which callers would otherwise have to special-case.
        let result = captured(0, "");
        assert_eq!(
            interpret_multi(result, &file_lines()),
            FzfMultiOutcome::Cancelled
        );
    }

    #[test]
    fn multi_exit_0_with_an_unmatched_line_is_unavailable() {
        let result = captured(0, "M  src/diff.rs\nnot a real candidate\n");
        assert!(matches!(
            interpret_multi(result, &file_lines()),
            FzfMultiOutcome::Unavailable(_)
        ));
    }

    #[test]
    fn multi_exit_1_no_match_is_cancelled() {
        let result = captured(1, "");
        assert_eq!(
            interpret_multi(result, &file_lines()),
            FzfMultiOutcome::Cancelled
        );
    }

    #[test]
    fn multi_exit_130_ctrl_c_or_esc_is_cancelled() {
        let result = captured(130, "");
        assert_eq!(
            interpret_multi(result, &file_lines()),
            FzfMultiOutcome::Cancelled
        );
    }

    #[test]
    fn multi_unrecognized_exit_code_is_unavailable() {
        let result = captured(2, "");
        assert!(matches!(
            interpret_multi(result, &file_lines()),
            FzfMultiOutcome::Unavailable(_)
        ));
    }

    #[test]
    fn multi_killed_by_a_signal_is_unavailable() {
        let result = Ok(Captured {
            success: false,
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
        assert!(matches!(
            interpret_multi(result, &file_lines()),
            FzfMultiOutcome::Unavailable(_)
        ));
    }

    #[test]
    fn multi_spawn_failure_is_unavailable() {
        let result: Result<Captured, ExecError> = Err(ExecError::Spawn(std::io::Error::other(
            "no such file or directory",
        )));
        assert!(matches!(
            interpret_multi(result, &file_lines()),
            FzfMultiOutcome::Unavailable(_)
        ));
    }

    #[test]
    fn select_files_against_the_real_subprocess_degrades_safely_with_no_terminal() {
        // Same rationale as `select_against_the_real_subprocess_degrades_safely_with_no_terminal`.
        if std::fs::File::open("/dev/tty").is_ok() {
            return;
        }
        let outcome = select_files(&std::env::current_dir().unwrap(), &file_lines());
        assert!(matches!(outcome, FzfMultiOutcome::Unavailable(_)));
    }
}
