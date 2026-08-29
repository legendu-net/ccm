//! Stage 6 of the check-order pipeline: diff enumeration/generation (prd.md "Diff
//! generation" and "Diff scope resolution"). Orchestrates `vcs::exec`/`vcs::argv`/
//! `vcs::scope`/`vcs::summary` and `progress` together, so it spans more than one
//! final exit code (2, via a mismatched `--include`/`--exclude` entry; 7, the
//! underlying subprocess failing; 8, nothing to diff) within this one spec-numbered
//! stage — unlike most modules, it returns [`CcmError`] directly rather than a single
//! narrow per-stage error type, precisely because the spec's own stage boundary
//! doesn't line up with a single exit code here.

use crate::error::{CcmError, DiffError, UsageError};
use crate::progress;
use crate::repo::RepoHandling;
use crate::vcs::{argv, exec, scope, summary};
use std::path::Path;

/// The generated diff text, plus (for jj) the resolved file scope — reused unchanged
/// for the later `jj commit`/`jj split` invocation so the commit matches what the
/// message describes.
#[derive(Debug)]
pub struct DiffResult {
    pub diff: String,
    pub files: Vec<String>,
}

/// Runs stage 6 to completion: for jj with `--include`/`--exclude`, first the `jj diff
/// --summary` enumeration call, then scope resolution, then the final `git diff`/`jj
/// diff` invocation.
///
/// # Errors
/// See the module-level docs: this can surface as exit 2, 7, or 8.
pub fn generate(
    handling: &RepoHandling,
    selection: &scope::Selection,
    cwd: &Path,
    stderr: &mut dyn std::io::Write,
) -> Result<DiffResult, CcmError> {
    match handling {
        RepoHandling::Git { .. } => {
            // Defense in depth: pipeline.rs's stage 3 already rejects
            // `--include`/`--exclude` under git handling before `generate` is ever
            // called, but `generate` is `pub` — a scope other than `All` reaching here
            // would otherwise be silently ignored (an unscoped full staged diff)
            // rather than surfacing the usage error it should.
            if !matches!(selection, scope::Selection::All) {
                return Err(UsageError::IncludeExcludeUnderGit.into());
            }
            generate_git(cwd, stderr)
        }
        RepoHandling::Jj { .. } => generate_jj(selection, cwd, stderr),
    }
}

fn generate_git(cwd: &Path, stderr: &mut dyn std::io::Write) -> Result<DiffResult, CcmError> {
    let args = argv::git_diff_args();
    let command = argv::render_command("git", &args);
    let _ = progress::generating_diff(stderr, &command);

    let captured = exec::run_simple("git", args, cwd).map_err(DiffError::Generation)?;
    if !captured.success {
        return Err(DiffError::Generation(stderr_text(captured)).into());
    }
    let _ = progress::section_break(stderr);

    if captured.stdout.is_empty() {
        return Err(DiffError::Empty.into());
    }
    Ok(DiffResult {
        diff: exec::owned_utf8_lossy(captured.stdout),
        files: vec![],
    })
}

fn generate_jj(
    selection: &scope::Selection,
    cwd: &Path,
    stderr: &mut dyn std::io::Write,
) -> Result<DiffResult, CcmError> {
    let files = match selection {
        scope::Selection::All => vec![],
        scope::Selection::Include(_) | scope::Selection::Exclude(_) => {
            let entries = enumerate_jj(cwd)?;
            scope::resolve_scope(&entries, selection)
                .map_err(|err| UsageError::UnmatchedPath { entry: err.entry })?
        }
        // Already resolved by the interactive file picker (`fileselect.rs`) — no
        // enumeration needed here; `resolve_scope` just dedups.
        scope::Selection::Explicit(_) => scope::resolve_scope(&[], selection)
            .map_err(|err| UsageError::UnmatchedPath { entry: err.entry })?,
    };

    // A requested scope (--include/--exclude) that resolves to nothing (e.g.
    // --exclude matched everything) is "nothing to diff" — short-circuit before
    // running `jj diff` at all, since a zero-argument `jj diff` would otherwise
    // silently diff the *whole* working copy instead, generating a message for
    // exactly what the user asked to exclude. Not an issue for Selection::All (there
    // "no files" correctly means "the whole working copy" from the start) or for
    // Selection::Include (every pattern is already required to have matched at least
    // one line, so `files` can't come back empty there).
    if !matches!(selection, scope::Selection::All) && files.is_empty() {
        return Err(DiffError::Empty.into());
    }

    // A section break separates whatever progress lines led up to this point — the
    // interactive file picker's own trailing output (`fileselect.rs`) for
    // `Selection::Explicit`, or nothing at all for `Include`/`Exclude` (the `jj diff
    // --summary` enumeration call above isn't itself progress-logged) — from the
    // diff-generation group about to start. Not needed for `Selection::All`, which has
    // no lead-up of its own.
    if !matches!(selection, scope::Selection::All) {
        let _ = progress::section_break(stderr);
    }

    let args = argv::jj_diff_args(&files);
    let command = argv::render_command("jj", &args);
    let _ = progress::generating_diff(stderr, &command);

    let captured = exec::run_simple("jj", args, cwd).map_err(DiffError::Generation)?;
    if !captured.success {
        return Err(DiffError::Generation(stderr_text(captured)).into());
    }
    let _ = progress::section_break(stderr);

    if captured.stdout.is_empty() {
        return Err(DiffError::Empty.into());
    }
    Ok(DiffResult {
        diff: exec::owned_utf8_lossy(captured.stdout),
        files,
    })
}

/// `pub(crate)` (rather than private) so `fileselect.rs` can reuse the same
/// enumeration call to build the interactive file picker's candidate list, instead of
/// duplicating it.
pub(crate) fn enumerate_jj(cwd: &Path) -> Result<Vec<summary::SummaryEntry>, CcmError> {
    let args = argv::jj_summary_args();
    let captured = exec::run_simple("jj", args, cwd).map_err(DiffError::Enumeration)?;
    if !captured.success {
        return Err(DiffError::Enumeration(stderr_text(captured)).into());
    }

    let text = exec::owned_utf8_lossy(captured.stdout);
    summary::parse_summary(&text).map_err(|err| DiffError::Enumeration(err.to_string()).into())
}

fn stderr_text(captured: exec::Captured) -> String {
    exec::owned_utf8_lossy(captured.stderr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::scope::Selection;

    fn git_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(tmp.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "t@example.com"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        tmp
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
    fn git_with_no_staged_changes_is_exit_8() {
        let repo = git_repo();
        let handling = RepoHandling::Git {
            root: repo.path().to_path_buf(),
        };
        let mut stderr = Vec::new();
        let err = generate(&handling, &Selection::All, repo.path(), &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
    }

    #[test]
    fn a_non_all_selection_under_git_handling_is_rejected_defensively() {
        // pipeline.rs's stage 3 is the normal guard against this; this pins that
        // `generate` itself refuses to silently ignore a scope under git handling if
        // ever called directly with one.
        let repo = git_repo();
        let handling = RepoHandling::Git {
            root: repo.path().to_path_buf(),
        };
        let selection = Selection::Include(vec![crate::vcs::pathnorm::normalize_str("a.txt")]);
        let mut stderr = Vec::new();
        let err = generate(&handling, &selection, repo.path(), &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 2);
    }

    #[test]
    fn git_with_staged_changes_returns_the_diff() {
        let repo = git_repo();
        std::fs::write(repo.path().join("a.txt"), "hello\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "a.txt"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        let handling = RepoHandling::Git {
            root: repo.path().to_path_buf(),
        };
        let mut stderr = Vec::new();
        let result = generate(&handling, &Selection::All, repo.path(), &mut stderr).unwrap();
        assert!(result.diff.contains("a.txt"));
        assert!(result.files.is_empty());
        let logged = String::from_utf8(stderr).unwrap();
        assert!(logged.contains("Generating diff using: git --no-pager diff --no-color --staged"));
    }

    #[test]
    fn git_diff_failure_is_exit_7() {
        // Not a git repository at all -> `git diff` itself fails.
        let tmp = tempfile::tempdir().unwrap();
        let handling = RepoHandling::Git {
            root: tmp.path().to_path_buf(),
        };
        let mut stderr = Vec::new();
        let err = generate(&handling, &Selection::All, tmp.path(), &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 7);
    }

    #[test]
    fn jj_with_no_changes_is_exit_8() {
        let (_tmp, repo) = jj_repo();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let mut stderr = Vec::new();
        let err = generate(&handling, &Selection::All, &repo, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
    }

    #[test]
    fn jj_with_changes_and_no_scope_returns_the_whole_diff() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let mut stderr = Vec::new();
        let result = generate(&handling, &Selection::All, &repo, &mut stderr).unwrap();
        assert!(result.diff.contains("a.txt"));
        assert!(result.files.is_empty());
    }

    #[test]
    fn jj_explicit_scope_skips_enumeration_and_diffs_only_those_files() {
        // `Selection::Explicit` — what the interactive file picker (`fileselect.rs`)
        // resolves to for a proper subset — must reach the same scoped `jj diff`
        // invocation `Include` does, but without an enumeration call of its own (the
        // picker already enumerated to build its candidate list).
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        std::fs::write(repo.join("b.txt"), "world\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let sel = Selection::Explicit(vec!["a.txt".to_string()]);
        let mut stderr = Vec::new();
        let result = generate(&handling, &sel, &repo, &mut stderr).unwrap();
        assert_eq!(result.files, vec!["a.txt".to_string()]);
        assert!(result.diff.contains("a.txt"));
        assert!(!result.diff.contains("b.txt"));
    }

    #[test]
    fn jj_include_resolves_scope_and_diffs_only_matched_files() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        std::fs::write(repo.join("b.txt"), "world\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let sel = Selection::from_cli(&[std::path::PathBuf::from("a.txt")], &[]);
        let mut stderr = Vec::new();
        let result = generate(&handling, &sel, &repo, &mut stderr).unwrap();
        assert_eq!(result.files, vec!["a.txt".to_string()]);
        assert!(result.diff.contains("a.txt"));
        assert!(!result.diff.contains("b.txt"));
    }

    #[test]
    fn jj_include_unmatched_path_is_exit_2() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let sel = Selection::from_cli(&[std::path::PathBuf::from("typo.txt")], &[]);
        let mut stderr = Vec::new();
        let err = generate(&handling, &sel, &repo, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 2);
    }

    #[test]
    fn jj_exclude_everything_is_exit_8_without_running_jj_diff() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let sel = Selection::from_cli(&[], &[std::path::PathBuf::from("a.txt")]);
        let mut stderr = Vec::new();
        let err = generate(&handling, &sel, &repo, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let logged = String::from_utf8(stderr).unwrap();
        assert!(!logged.contains("Generating diff using:"));
    }
}
