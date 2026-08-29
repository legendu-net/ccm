//! The interactive file picker (prd.md "Diff scope resolution"): a default-mode,
//! jj-only prompt asking whether to restrict the diff to specific files before diff
//! generation runs, and — when the answer is yes — an `fzf` multi-select (with a
//! per-file diff preview) or, when `fzf` isn't available, a numbered stdin fallback,
//! over the working copy's changed files. Skipped entirely — no prompt at all — when
//! the working copy has at most one changed file: with nothing to actually choose
//! between, asking would be pointless.
//!
//! Reuses the same enumeration call `diff::generate`'s jj `--include`/`--exclude`
//! path uses (`diff::enumerate_jj`, `vcs::summary::parse_summary`), so the candidate
//! list is exactly what `--include`/`--exclude` would validate paths against — this
//! module and `Selection::Include`/`Exclude` describe the same working copy, just
//! discovered differently (typed patterns vs. picked from a list).

use crate::diff;
use crate::error::CcmError;
use crate::fzf;
use crate::picker;
use crate::progress;
use crate::vcs::scope::{self, Selection};
use crate::vcs::summary::SummaryEntry;
use std::io::BufRead;
use std::path::Path;

/// Enumerates the working copy and resolves it to a [`Selection`], prompting
/// interactively only when there's an actual choice to make:
/// - At most one changed file (zero or one) -> [`Selection::All`] immediately, no
///   prompt shown at all: restricting to a subset and diffing the whole working copy
///   already mean the same thing then, so there's nothing worth asking about. Zero
///   changed files still surfaces as the usual "nothing to diff" exit 8 once
///   `diff::generate` runs on the resulting `Selection::All`.
/// - Two or more changed files, then No (the default, a bare Enter) -> [`Selection::All`].
/// - Two or more changed files, then Yes, then any marked subset (including every
///   candidate) -> [`Selection::Explicit`] with exactly those paths, in candidate
///   (first-seen enumeration) order. Marking every candidate deliberately does *not*
///   collapse to [`Selection::All`]: the two aren't equivalent once time is allowed to
///   pass between enumeration and the eventual `jj diff`/`jj commit` invocations (an
///   interactive picker can leave the working copy open for arbitrarily long) —
///   `Selection::All` re-resolves to whatever the working copy *currently* contains at
///   each of those later points, which could by then include a file that was never
///   enumerated and that the user never saw or chose, where pinning the exact
///   enumerated paths cannot. The tradeoff is that marking every candidate still flips
///   the jj commit-command picker from `[D]escribe` to `[S]plit` (see `commit.rs`),
///   even though nothing was excluded — the accepted cost of not silently including a
///   file the user was never shown.
///
/// `fzf_enabled` is `!fzf::disabled_by_env(...)` — the caller (`pipeline.rs`) only
/// calls this function at all once its own `env.stdin_is_terminal()` guard has
/// already passed, the same terminal check stage 5's tool picker folds into its own
/// `fzf_enabled` computation; here it's just checked one call frame up instead. `raw`
/// is forwarded to the yes/no prompt only — `fzf` and the numbered fallback have no
/// raw-mode concept of their own.
///
/// # Errors
/// Any error [`diff::enumerate_jj`] itself can surface (exit 7); otherwise
/// [`crate::error::PickerCancelled`] (exit 14) if either prompt is cancelled.
pub fn resolve_interactively(
    cwd: &Path,
    stdin: &mut dyn BufRead,
    stderr: &mut dyn std::io::Write,
    fzf_picker: &dyn fzf::Fzf,
    fzf_enabled: bool,
    raw: bool,
) -> Result<Selection, CcmError> {
    let entries = diff::enumerate_jj(cwd)?;
    let targets = dedup_targets(&entries);
    if targets.len() <= 1 {
        return Ok(Selection::All);
    }

    let restrict = picker::prompt_yes_no(stdin, stderr, raw).map_err(picker::to_ccm_error)?;
    let _ = progress::section_break(stderr);
    if !restrict {
        return Ok(Selection::All);
    }

    let lines = candidate_lines(&entries, &targets);
    let indices = if fzf_enabled {
        match fzf_picker.select_files(cwd, &lines) {
            fzf::FzfMultiOutcome::Selected(indices) => indices,
            fzf::FzfMultiOutcome::Cancelled => {
                return Err(picker::to_ccm_error(picker::PickerError::Cancelled));
            }
            fzf::FzfMultiOutcome::Unavailable(reason) => {
                let _ = progress::fzf_unavailable(stderr, &reason);
                picker::prompt_indices(stdin, stderr, &lines).map_err(picker::to_ccm_error)?
            }
        }
    } else {
        picker::prompt_indices(stdin, stderr, &lines).map_err(picker::to_ccm_error)?
    };
    let _ = progress::section_break(stderr);

    // Pinned as `Explicit` even when every candidate was marked — never collapsed
    // back to `Selection::All` (see this fn's doc for why).
    let selected: Vec<String> = indices.into_iter().map(|i| targets[i].clone()).collect();
    let _ = progress::files_selected(stderr, &selected);
    Ok(Selection::Explicit(selected))
}

/// The deduplicated, first-seen-order list of `SummaryEntry::target` paths — the same
/// contribution rule `vcs::scope::resolve_scope` uses for `Selection::All` (see that
/// module's doc comment for why `target`, never `source`, is what a line contributes),
/// via the same `dedup_preserve_order` helper that rule is built on.
fn dedup_targets(entries: &[SummaryEntry]) -> Vec<String> {
    scope::dedup_preserve_order(entries.iter().map(|entry| entry.target.clone()))
}

/// Builds one `"<status>  <target>"` display/candidate line per deduplicated target,
/// pairing each with the status of its first occurrence in `entries` (a target only
/// ever appears once in `targets`, by construction of [`dedup_targets`]).
fn candidate_lines(entries: &[SummaryEntry], targets: &[String]) -> Vec<String> {
    targets
        .iter()
        .map(|target| {
            let status = entries
                .iter()
                .find(|entry| &entry.target == target)
                .map_or('?', |entry| entry.status);
            format!("{status}  {target}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::summary::parse_summary;

    fn entries(text: &str) -> Vec<SummaryEntry> {
        parse_summary(text).unwrap()
    }

    #[test]
    fn dedup_targets_preserves_first_seen_order_and_drops_duplicates() {
        let entries = entries("M a.rs\nA b.rs\nM a.rs\n");
        assert_eq!(
            dedup_targets(&entries),
            vec!["a.rs".to_string(), "b.rs".to_string()]
        );
    }

    #[test]
    fn candidate_lines_pair_status_with_target() {
        let entries = entries("M a.rs\nA b.rs\n");
        let targets = dedup_targets(&entries);
        assert_eq!(
            candidate_lines(&entries, &targets),
            vec!["M  a.rs".to_string(), "A  b.rs".to_string()]
        );
    }

    #[test]
    fn candidate_lines_use_the_rename_targets_new_name() {
        let entries = entries("R src/{old.rs => new.rs}\n");
        let targets = dedup_targets(&entries);
        assert_eq!(targets, vec!["src/new.rs".to_string()]);
        assert_eq!(
            candidate_lines(&entries, &targets),
            vec!["R  src/new.rs".to_string()]
        );
    }

    // ---- resolve_interactively ----

    struct PanickingFzf;
    impl fzf::Fzf for PanickingFzf {
        fn select(&self, _cwd: &Path, _lines: &[String]) -> fzf::FzfOutcome {
            panic!("select should not have been invoked in this test");
        }
        fn select_files(&self, _cwd: &Path, _lines: &[String]) -> fzf::FzfMultiOutcome {
            panic!("select_files should not have been invoked in this test");
        }
    }

    struct FakeFilesFzf(fzf::FzfMultiOutcome);
    impl fzf::Fzf for FakeFilesFzf {
        fn select(&self, _cwd: &Path, _lines: &[String]) -> fzf::FzfOutcome {
            panic!("select should not have been invoked in this test");
        }
        fn select_files(&self, _cwd: &Path, _lines: &[String]) -> fzf::FzfMultiOutcome {
            self.0.clone()
        }
    }

    fn jj_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let jj_config = tmp.path().join("jj.toml");
        std::fs::write(
            &jj_config,
            "[user]\nname = \"t\"\nemail = \"t@example.com\"\n",
        )
        .unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::process::Command::new("jj")
            .args(["git", "init", "--no-colocate"])
            .current_dir(&repo)
            .env("JJ_CONFIG", &jj_config)
            .status()
            .unwrap();
        (tmp, repo)
    }

    #[test]
    fn zero_changed_files_skips_the_prompt_and_returns_all() {
        let (_tmp, repo) = jj_repo();
        // No stdin at all: if the prompt were shown, EOF would cancel it (exit 14)
        // rather than returning a selection.
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut stderr = Vec::new();
        let selection =
            resolve_interactively(&repo, &mut stdin, &mut stderr, &PanickingFzf, true, false)
                .unwrap();
        assert_eq!(selection, Selection::All);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Restrict the diff to specific files?"));
    }

    #[test]
    fn one_changed_file_skips_the_prompt_and_returns_all() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut stderr = Vec::new();
        let selection =
            resolve_interactively(&repo, &mut stdin, &mut stderr, &PanickingFzf, true, false)
                .unwrap();
        assert_eq!(selection, Selection::All);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Restrict the diff to specific files?"));
    }

    #[test]
    fn two_changed_files_no_answer_returns_all() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let mut stdin = std::io::Cursor::new(b"n\n".to_vec());
        let mut stderr = Vec::new();
        let selection =
            resolve_interactively(&repo, &mut stdin, &mut stderr, &PanickingFzf, true, false)
                .unwrap();
        assert_eq!(selection, Selection::All);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("Restrict the diff to specific files?"));
    }

    #[test]
    fn two_changed_files_no_answer_is_exit_14() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut stderr = Vec::new();
        let err = resolve_interactively(&repo, &mut stdin, &mut stderr, &PanickingFzf, true, false)
            .unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
    }

    #[test]
    fn fzf_selecting_every_candidate_pins_the_explicit_list_rather_than_collapsing() {
        // Marking every candidate must NOT collapse to `Selection::All`: a file that
        // starts changing after enumeration (but before the eventual `jj diff`/`jj
        // commit` calls) would otherwise be silently swept into an unrestricted
        // `Selection::All`, even though the user never saw or chose it. Pinning the
        // exact enumerated set protects against that.
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let mut stdin = std::io::Cursor::new(b"y\n".to_vec());
        let mut stderr = Vec::new();
        let fzf_picker = FakeFilesFzf(fzf::FzfMultiOutcome::Selected(vec![0, 1]));
        let selection =
            resolve_interactively(&repo, &mut stdin, &mut stderr, &fzf_picker, true, false)
                .unwrap();
        assert_eq!(
            selection,
            Selection::Explicit(vec!["a.rs".to_string(), "b.rs".to_string()])
        );
    }

    #[test]
    fn fzf_selecting_a_subset_returns_explicit_files() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let mut stdin = std::io::Cursor::new(b"y\n".to_vec());
        let mut stderr = Vec::new();
        let fzf_picker = FakeFilesFzf(fzf::FzfMultiOutcome::Selected(vec![0]));
        let selection =
            resolve_interactively(&repo, &mut stdin, &mut stderr, &fzf_picker, true, false)
                .unwrap();
        assert_eq!(selection, Selection::Explicit(vec!["a.rs".to_string()]));
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("a.rs"));
    }

    #[test]
    fn fzf_cancelled_is_exit_14() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let mut stdin = std::io::Cursor::new(b"y\n".to_vec());
        let mut stderr = Vec::new();
        let fzf_picker = FakeFilesFzf(fzf::FzfMultiOutcome::Cancelled);
        let err = resolve_interactively(&repo, &mut stdin, &mut stderr, &fzf_picker, true, false)
            .unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
    }

    #[test]
    fn fzf_unavailable_falls_back_to_the_numbered_prompt() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let mut stdin = std::io::Cursor::new(b"y\n0\n".to_vec());
        let mut stderr = Vec::new();
        let fzf_picker = FakeFilesFzf(fzf::FzfMultiOutcome::Unavailable("fzf not found".into()));
        let selection =
            resolve_interactively(&repo, &mut stdin, &mut stderr, &fzf_picker, true, false)
                .unwrap();
        assert_eq!(selection, Selection::Explicit(vec!["a.rs".to_string()]));
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("fzf unavailable"));
        assert!(printed.contains("Enter indices"));
    }

    #[test]
    fn fzf_disabled_goes_straight_to_the_numbered_prompt() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let mut stdin = std::io::Cursor::new(b"y\n0\n".to_vec());
        let mut stderr = Vec::new();
        let selection =
            resolve_interactively(&repo, &mut stdin, &mut stderr, &PanickingFzf, false, false)
                .unwrap();
        assert_eq!(selection, Selection::Explicit(vec!["a.rs".to_string()]));
    }

    #[test]
    fn numbered_prompt_blank_line_selects_every_candidate_explicitly() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let mut stdin = std::io::Cursor::new(b"y\n\n".to_vec());
        let mut stderr = Vec::new();
        let selection =
            resolve_interactively(&repo, &mut stdin, &mut stderr, &PanickingFzf, false, false)
                .unwrap();
        assert_eq!(
            selection,
            Selection::Explicit(vec!["a.rs".to_string(), "b.rs".to_string()])
        );
    }
}
