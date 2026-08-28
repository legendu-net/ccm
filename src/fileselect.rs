//! The interactive file picker (prd.md "Diff scope resolution"): a default-mode,
//! jj-only prompt asking whether to restrict the diff to specific files before diff
//! generation runs, and — when the answer is yes — an `fzf` multi-select (with a
//! per-file diff preview) or, when `fzf` isn't available, a numbered stdin fallback,
//! over the working copy's changed files.
//!
//! Reuses the same enumeration call `diff::generate`'s jj `--include`/`--exclude`
//! path uses (`diff::enumerate_jj`, `vcs::summary::parse_summary`), so the candidate
//! list is exactly what `--include`/`--exclude` would validate paths against — this
//! module and `Selection::Include`/`Exclude` describe the same working copy, just
//! discovered differently (typed patterns vs. picked from a list).

use crate::diff;
use crate::error::{CcmError, DiffError};
use crate::fzf;
use crate::picker;
use crate::progress;
use crate::vcs::scope::{self, Selection};
use crate::vcs::summary::SummaryEntry;
use std::io::BufRead;
use std::path::Path;

/// Runs the "restrict the diff to specific files?" prompt to completion and resolves
/// it to a [`Selection`]:
/// - No (the default, a bare Enter) -> [`Selection::All`], nothing enumerated or
///   logged — identical to a run that never triggered this prompt at all.
/// - Yes, then every candidate marked -> also [`Selection::All`]: this collapse keeps
///   "select everything" indistinguishable from declining, rather than making
///   `files` non-empty for no real restriction — which would otherwise flip the jj
///   commit-command picker from `[D]escribe` to `[S]plit` (see `commit.rs`) and `jj
///   split` the entire working copy for nothing.
/// - Yes, then a proper subset marked -> [`Selection::Explicit`] with those paths, in
///   candidate (first-seen enumeration) order.
///
/// `fzf_enabled` is `!fzf::disabled_by_env(...)` — the caller (`pipeline.rs`) only
/// calls this function at all once its own `env.stdin_is_terminal()` guard has
/// already passed, the same terminal check stage 5's tool picker folds into its own
/// `fzf_enabled` computation; here it's just checked one call frame up instead. `raw`
/// is forwarded to the yes/no prompt only — `fzf` and the numbered fallback have no
/// raw-mode concept of their own.
///
/// # Errors
/// [`crate::error::PickerCancelled`] (exit 14) if either prompt is cancelled;
/// [`DiffError::Empty`] (exit 8) if the working copy has nothing changed to enumerate
/// once "yes" is answered — showing an empty picker would be pointless; any error
/// [`diff::enumerate_jj`] itself can surface (exit 7).
pub fn resolve_interactively(
    cwd: &Path,
    stdin: &mut dyn BufRead,
    stderr: &mut dyn std::io::Write,
    fzf_picker: &dyn fzf::Fzf,
    fzf_enabled: bool,
    raw: bool,
) -> Result<Selection, CcmError> {
    let restrict = picker::prompt_yes_no(stdin, stderr, raw).map_err(picker::to_ccm_error)?;
    let _ = progress::section_break(stderr);
    if !restrict {
        return Ok(Selection::All);
    }

    let entries = diff::enumerate_jj(cwd, stderr)?;
    if entries.is_empty() {
        return Err(DiffError::Empty.into());
    }
    let targets = dedup_targets(&entries);
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

    if indices.len() == targets.len() {
        // Every candidate marked -> equivalent to declining (see this fn's doc).
        return Ok(Selection::All);
    }
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
    fn no_answer_returns_all_without_enumerating() {
        let (_tmp, repo) = jj_repo();
        // Deliberately no files written: if this enumerated, the (still-empty)
        // working copy would surface as DiffError::Empty rather than `Selection::All`.
        let mut stdin = std::io::Cursor::new(b"n\n".to_vec());
        let mut stderr = Vec::new();
        let selection =
            resolve_interactively(&repo, &mut stdin, &mut stderr, &PanickingFzf, true, false)
                .unwrap();
        assert_eq!(selection, Selection::All);
    }

    #[test]
    fn yes_then_no_changes_is_exit_8() {
        let (_tmp, repo) = jj_repo();
        let mut stdin = std::io::Cursor::new(b"y\n".to_vec());
        let mut stderr = Vec::new();
        let err = resolve_interactively(&repo, &mut stdin, &mut stderr, &PanickingFzf, true, false)
            .unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
    }

    #[test]
    fn yes_no_answer_is_exit_14() {
        let (_tmp, repo) = jj_repo();
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut stderr = Vec::new();
        let err = resolve_interactively(&repo, &mut stdin, &mut stderr, &PanickingFzf, true, false)
            .unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
    }

    #[test]
    fn fzf_selecting_every_candidate_collapses_to_all() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let mut stdin = std::io::Cursor::new(b"y\n".to_vec());
        let mut stderr = Vec::new();
        let fzf_picker = FakeFilesFzf(fzf::FzfMultiOutcome::Selected(vec![0, 1]));
        let selection =
            resolve_interactively(&repo, &mut stdin, &mut stderr, &fzf_picker, true, false)
                .unwrap();
        assert_eq!(selection, Selection::All);
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
    fn numbered_prompt_blank_line_selects_all_which_collapses() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let mut stdin = std::io::Cursor::new(b"y\n\n".to_vec());
        let mut stderr = Vec::new();
        let selection =
            resolve_interactively(&repo, &mut stdin, &mut stderr, &PanickingFzf, false, false)
                .unwrap();
        assert_eq!(selection, Selection::All);
    }
}
