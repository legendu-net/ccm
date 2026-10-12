//! Stage 6 of the check-order pipeline: diff enumeration/generation (prd.md "Diff
//! generation" and "Diff scope resolution"). Orchestrates `vcs::exec`/`vcs::argv`/
//! `vcs::scope`/`vcs::summary` and `progress` together, so it spans more than one
//! final exit code (2, via a mismatched `--include`/`--exclude` entry; 7, the
//! underlying subprocess failing; 8, nothing to diff) within this one spec-numbered
//! stage — unlike most modules, it returns [`CcmError`] directly rather than a single
//! narrow per-stage error type, precisely because the spec's own stage boundary
//! doesn't line up with a single exit code here. The one exception is
//! [`check_revision`], which pipeline.rs calls at stage 3 instead.

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
    /// Empty when unscoped. Under `--revision`, a scope covering every changed file of
    /// that revision also comes back empty: `jj split -r` with everything selected would
    /// leave an empty, undescribed revision behind, so such a scope is recorded unscoped
    /// with `jj describe -r` instead (prd.md "jj commit commands").
    pub files: Vec<String>,
}

/// Runs stage 6 to completion: for jj with `--include`/`--exclude`, first the `jj diff`
/// enumeration call, then scope resolution, then the final `git diff`/`jj diff`
/// invocation. `revision` is `--revision`'s value (jj only — as returned by
/// [`check_revision`]); `None` diffs the working copy.
///
/// # Errors
/// See the module-level docs: this can surface as exit 2, 7, or 8.
pub fn generate(
    handling: &RepoHandling,
    selection: &scope::Selection,
    revision: Option<argv::Revision<'_>>,
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
            // Same defense in depth for `--revision`.
            if revision.is_some() {
                return Err(UsageError::RevisionUnderGit.into());
            }
            generate_git(cwd, stderr)
        }
        RepoHandling::Jj { .. } => generate_jj(selection, revision, cwd, stderr),
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
    revision: Option<argv::Revision<'_>>,
    cwd: &Path,
    stderr: &mut dyn std::io::Write,
) -> Result<DiffResult, CcmError> {
    let mut enumerated = None;
    let files = match selection {
        scope::Selection::All => vec![],
        scope::Selection::Include(_) | scope::Selection::Exclude(_) => {
            let entries = enumerate_jj(revision, cwd)?;
            let files = scope::resolve_scope(&entries, selection)
                .map_err(|err| UsageError::UnmatchedPath { entry: err.entry })?;
            enumerated = Some(entries);
            files
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
    // `Selection::Explicit`, or nothing at all for `Include`/`Exclude` (the `jj diff`
    // enumeration call above isn't itself progress-logged) — from the diff-generation
    // group about to start. Not needed for `Selection::All`, which has no lead-up of
    // its own.
    if !matches!(selection, scope::Selection::All) {
        let _ = progress::section_break(stderr);
    }

    // See `DiffResult::files`. The interactive file picker's `Selection::Explicit`
    // arrives without an enumeration of its own here, so it's enumerated again —
    // deliberately not reusing the picker's own candidate enumeration: a file that
    // was added to the revision while the picker was open (written into `@`, or squashed
    // into the revision by a concurrent jj operation) makes this check fail, so the
    // run falls back to `jj split` (leaving that unseen file out of the described
    // revision) instead of `jj describe`-ing a revision whose full contents the message
    // never covered (design.md "Interactive file selection: why 'select everything' is
    // not 'decline'").
    let covers_revision = match revision {
        Some(revision) if !files.is_empty() => {
            let entries = match enumerated {
                Some(entries) => entries,
                None => enumerate_jj(Some(revision), cwd)?,
            };
            covers(&entries, &files)
        }
        _ => false,
    };

    let args = argv::jj_diff_args(revision, &files);
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
        files: if covers_revision { vec![] } else { files },
    })
}

/// Whether `files` names every changed file `entries` enumerates.
fn covers(entries: &[summary::SummaryEntry], files: &[String]) -> bool {
    let scoped: std::collections::HashSet<&str> = files.iter().map(String::as_str).collect();
    entries
        .iter()
        .all(|entry| scoped.contains(entry.target.as_str()))
}

/// Checks that `--revision`'s `revision` resolves to exactly one jj revision, and that
/// it's mutable (prd.md "Diff generation"). pipeline.rs runs it at stage 3, before
/// any picker prompt or anything in stage 6 — so a revset that names no revision or
/// several, or an immutable one, is rejected up front, rather than surfacing later as a combined multi-revision diff, or as a `jj
/// describe`/`jj split` failure only after the user has already reviewed/edited the
/// message. Not progress-logged, same as the enumeration call. Returns the checked
/// revision, recording whether it's the working-copy revision.
///
/// # Errors
/// [`DiffError::Revision`] (exit 7) if the `jj log` call itself fails (e.g. `jj` not
/// found, a syntactically invalid revset, an unknown revision) or its output isn't the
/// expected `<commit id> true|false true|false` line;
/// [`UsageError::RevisionNotSingle`] (exit 2) if it succeeds but doesn't resolve to
/// exactly one revision, or fails because the revset references a divergent change id;
/// [`UsageError::RevisionImmutable`] (exit 2) if that revision is immutable.
pub fn check_revision<'a>(revision: &'a str, cwd: &Path) -> Result<argv::Revision<'a>, CcmError> {
    let args = argv::jj_revision_check_args(revision);
    let captured = exec::run_simple("jj", args, cwd).map_err(DiffError::Revision)?;
    if !captured.success {
        let message = stderr_text(captured);
        // A divergent change id names several revisions, but jj rejects any revset
        // referencing one instead of listing them.
        if is_divergence_error(&message) {
            return Err(UsageError::RevisionNotSingle {
                revision: revision.to_string(),
            }
            .into());
        }
        return Err(DiffError::Revision(message).into());
    }
    let text = exec::owned_utf8_lossy(captured.stdout);
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.len() != 1 {
        return Err(UsageError::RevisionNotSingle {
            revision: revision.to_string(),
        }
        .into());
    }
    let (immutable, is_working_copy) = parse_flags(lines[0])?;
    if immutable {
        return Err(UsageError::RevisionImmutable {
            revision: revision.to_string(),
        }
        .into());
    }
    Ok(argv::Revision {
        revset: revision,
        is_working_copy,
    })
}

/// Whether jj's stderr reports a revset referencing a divergent change id ("Error:
/// Change ID `<id>` is divergent" as of jj 0.45.1). jj has no machine-readable form of
/// this, so only the keyword is matched (tolerating rewording), on `Error:` lines only
/// (not hints such as "Did you mean `divergent-fix`?") and outside backtick-quoted
/// spans (where jj echoes the revset and other names, which may say "divergent"
/// themselves).
fn is_divergence_error(stderr: &str) -> bool {
    stderr
        .lines()
        // An odd backtick count means an echoed name contained a backtick itself (jj
        // doesn't escape them), so which spans are quoted can't be told apart.
        .filter(|line| line.starts_with("Error:") && line.matches('`').count() % 2 == 0)
        .any(|line| {
            line.split('`')
                .step_by(2)
                .any(|unquoted| unquoted.to_lowercase().contains("divergent"))
        })
}

/// Parses one line of [`check_revision`]'s `jj log` output, `<commit id> true|false
/// true|false`, into its `immutable` and `current_working_copy` flags. Anything else
/// (e.g. a user template alias shadowing `immutable`/`current_working_copy`) is an
/// error rather than silently counting as mutable.
fn parse_flags(line: &str) -> Result<(bool, bool), DiffError> {
    let parse = |flag| match flag {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };
    let mut fields = line.trim_end().split(' ');
    match (
        fields.next(),
        fields.next().and_then(parse),
        fields.next().and_then(parse),
        fields.next(),
    ) {
        (Some(_), Some(immutable), Some(is_working_copy), None) => Ok((immutable, is_working_copy)),
        _ => Err(DiffError::Revision(format!(
            "unexpected `jj log` output: {line:?}"
        ))),
    }
}

/// `pub(crate)` (rather than private) so `fileselect.rs` can reuse the same
/// enumeration call to build the interactive file picker's candidate list, instead of
/// duplicating it. `revision` is `--revision`'s value; `None` enumerates the working
/// copy.
pub(crate) fn enumerate_jj(
    revision: Option<argv::Revision<'_>>,
    cwd: &Path,
) -> Result<Vec<summary::SummaryEntry>, CcmError> {
    let args = argv::jj_enumerate_args(revision);
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
    use crate::testutil::{PARENT, jj_log, jj_new, run_jj};
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
        let err = generate(&handling, &Selection::All, None, repo.path(), &mut stderr).unwrap_err();
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
        let err = generate(&handling, &selection, None, repo.path(), &mut stderr).unwrap_err();
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
        let result = generate(&handling, &Selection::All, None, repo.path(), &mut stderr).unwrap();
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
        let err = generate(&handling, &Selection::All, None, tmp.path(), &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 7);
    }

    #[test]
    fn jj_with_no_changes_is_exit_8() {
        let (_tmp, repo) = jj_repo();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let mut stderr = Vec::new();
        let err = generate(&handling, &Selection::All, None, &repo, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
    }

    #[test]
    fn jj_with_changes_and_no_scope_returns_the_whole_diff() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let mut stderr = Vec::new();
        let result = generate(&handling, &Selection::All, None, &repo, &mut stderr).unwrap();
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
        let result = generate(&handling, &sel, None, &repo, &mut stderr).unwrap();
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
        let result = generate(&handling, &sel, None, &repo, &mut stderr).unwrap();
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
        let err = generate(&handling, &sel, None, &repo, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 2);
    }

    #[test]
    fn jj_exclude_everything_is_exit_8_without_running_jj_diff() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let sel = Selection::from_cli(&[], &[std::path::PathBuf::from("a.txt")]);
        let mut stderr = Vec::new();
        let err = generate(&handling, &sel, None, &repo, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let logged = String::from_utf8(stderr).unwrap();
        assert!(!logged.contains("Generating diff using:"));
    }

    #[test]
    fn jj_revision_diffs_that_revision_instead_of_the_working_copy() {
        let (tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        jj_new(&tmp, &repo);
        std::fs::write(repo.join("b.txt"), "world\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let mut stderr = Vec::new();
        let result =
            generate(&handling, &Selection::All, Some(PARENT), &repo, &mut stderr).unwrap();
        assert!(result.diff.contains("a.txt"));
        assert!(!result.diff.contains("b.txt"));
        let logged = String::from_utf8(stderr).unwrap();
        assert!(logged.contains(
            "Generating diff using: jj --no-pager --ignore-working-copy diff '--color=never' -r @-"
        ));
    }

    #[test]
    fn jj_revision_include_is_matched_against_that_revisions_files() {
        let (tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        jj_new(&tmp, &repo);
        std::fs::write(repo.join("b.txt"), "world\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let mut stderr = Vec::new();

        let sel = Selection::from_cli(&[std::path::PathBuf::from("a.txt")], &[]);
        let result = generate(&handling, &sel, Some(PARENT), &repo, &mut stderr).unwrap();
        // a.txt is the only file @- changed, so the scope covers the whole revision.
        assert!(result.files.is_empty());

        // b.txt only changed in the working copy, not in @-.
        let sel = Selection::from_cli(&[std::path::PathBuf::from("b.txt")], &[]);
        let err = generate(&handling, &sel, Some(PARENT), &repo, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 2);
    }

    #[test]
    fn a_revision_under_git_handling_is_rejected_defensively() {
        let repo = git_repo();
        let handling = RepoHandling::Git {
            root: repo.path().to_path_buf(),
        };
        let mut stderr = Vec::new();
        let err = generate(
            &handling,
            &Selection::All,
            Some(argv::Revision {
                revset: "HEAD",
                is_working_copy: false,
            }),
            repo.path(),
            &mut stderr,
        )
        .unwrap_err();
        assert!(matches!(err, CcmError::Usage(UsageError::RevisionUnderGit)));
    }

    #[test]
    fn check_revision_accepts_exactly_one_revision() {
        let (tmp, repo) = jj_repo();
        // Without this, `@-` is `root()`, which is immutable.
        jj_new(&tmp, &repo);
        assert!(check_revision("@", &repo).unwrap().is_working_copy);
        assert!(!check_revision("@-", &repo).unwrap().is_working_copy);
        // The Neovim wrapper passes a change id, not a literal `@`.
        let change_id = jj_log(&tmp, &repo, "@", "change_id");
        assert!(check_revision(&change_id, &repo).unwrap().is_working_copy);
    }

    #[test]
    fn check_revision_rejects_zero_or_several_revisions_as_exit_2() {
        let (_tmp, repo) = jj_repo();
        for revset in ["none()", "all()"] {
            let err = check_revision(revset, &repo).unwrap_err();
            assert!(
                matches!(&err, CcmError::Usage(UsageError::RevisionNotSingle { revision }) if revision == revset),
                "{revset}: {err:?}"
            );
        }
    }

    #[test]
    fn check_revision_rejects_a_divergent_change_id_as_exit_2() {
        let (tmp, repo) = jj_repo();
        let change_id = jj_log(&tmp, &repo, "@", "change_id");
        let op_id = run_jj(
            &tmp,
            &repo,
            &["op", "log", "--no-graph", "--limit", "1", "-T", "id"],
        );
        // Two concurrent rewrites of the same change leave it divergent.
        run_jj(&tmp, &repo, &["describe", "-m", "one"]);
        run_jj(&tmp, &repo, &["--at-op", &op_id, "describe", "-m", "two"]);

        let err = check_revision(&change_id, &repo).unwrap_err();
        assert!(
            matches!(&err, CcmError::Usage(UsageError::RevisionNotSingle { revision }) if *revision == change_id),
            "{err:?}"
        );
        assert_eq!(err.exit_code().as_u8(), 2);

        // jj echoes the revset back in other errors; that alone isn't divergence.
        let err = check_revision("\"x is divergent\"", &repo).unwrap_err();
        assert!(
            matches!(&err, CcmError::Diff(DiffError::Revision(_))),
            "{err:?}"
        );
        assert_eq!(err.exit_code().as_u8(), 7);
    }

    #[test]
    fn check_revision_rejects_an_immutable_revision_as_exit_2() {
        let (_tmp, repo) = jj_repo();
        let err = check_revision("root()", &repo).unwrap_err();
        assert!(
            matches!(&err, CcmError::Usage(UsageError::RevisionImmutable { revision }) if revision == "root()"),
            "{err:?}"
        );
        assert_eq!(err.exit_code().as_u8(), 2);
    }

    #[test]
    fn is_divergence_error_matches_only_jjs_own_error_text() {
        assert!(is_divergence_error(
            "Error: Change ID `t` is divergent\nHint: Use change offset to select single \
             revision: t/0, t/1\n"
        ));
        assert!(!is_divergence_error(
            "Error: Revision `divergent-fx` doesn't exist\nHint: Did you mean `divergent-fix`?\n"
        ));
        assert!(!is_divergence_error(
            "Error: Revision `\"x is divergent\"` doesn't exist\n"
        ));
        assert!(!is_divergence_error(
            "Error: Revision `\"a`b divergent\"` doesn't exist\n"
        ));
        assert!(!is_divergence_error(
            "Error: Failed to parse revset: Syntax error\nCaused by:  --> 1:5\n  |\n1 | foo \
             divergent\n"
        ));
    }

    #[test]
    fn parse_flags_accepts_only_true_or_false() {
        assert_eq!(parse_flags("abc123 true false").unwrap(), (true, false));
        assert_eq!(parse_flags("abc123 false true\r").unwrap(), (false, true));
        for line in [
            "abc123",
            "abc123 true",
            "abc123 yes false",
            "abc123 True false",
            "abc123 false false extra",
            "true false",
        ] {
            assert!(
                matches!(parse_flags(line), Err(DiffError::Revision(_))),
                "{line:?}"
            );
        }
    }

    #[test]
    fn check_revision_unknown_revision_is_exit_7() {
        let (_tmp, repo) = jj_repo();
        let err = check_revision("no_such_bookmark_xyz", &repo).unwrap_err();
        assert!(
            matches!(err, CcmError::Diff(DiffError::Revision(_))),
            "{err:?}"
        );
        assert_eq!(err.exit_code().as_u8(), 7);
    }

    #[test]
    fn a_scope_covering_the_revision_comes_back_unscoped() {
        let (tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        std::fs::write(repo.join("b.txt"), "world\n").unwrap();
        jj_new(&tmp, &repo);
        let handling = RepoHandling::Jj { root: repo.clone() };
        let mut stderr = Vec::new();

        let some = Selection::Explicit(vec!["a.txt".to_string()]);
        let result = generate(&handling, &some, Some(PARENT), &repo, &mut stderr).unwrap();
        assert_eq!(result.files, vec!["a.txt".to_string()]);

        // The interactive file picker's `Explicit` scope is enumerated again to check.
        let every = Selection::Explicit(vec!["a.txt".to_string(), "b.txt".to_string()]);
        let result = generate(&handling, &every, Some(PARENT), &repo, &mut stderr).unwrap();
        assert!(result.files.is_empty());

        // So is `-r @`'s.
        std::fs::write(repo.join("c.txt"), "!\n").unwrap();
        let every_wc = Selection::Explicit(vec!["c.txt".to_string()]);
        let working_copy = Some(argv::Revision {
            revset: "@",
            is_working_copy: true,
        });
        let result = generate(&handling, &every_wc, working_copy, &repo, &mut stderr).unwrap();
        assert!(result.files.is_empty());

        // Never collapsed without --revision, even when the scope is everything.
        let result = generate(&handling, &every_wc, None, &repo, &mut stderr).unwrap();
        assert_eq!(result.files, vec!["c.txt".to_string()]);
    }

    #[test]
    fn covering_the_revision_counts_a_rename_by_its_new_name() {
        let (tmp, repo) = jj_repo();
        let body = "hello world, this is a rename test with enough content to be detected\n";
        std::fs::write(repo.join("old.txt"), body).unwrap();
        jj_new(&tmp, &repo);
        std::fs::rename(repo.join("old.txt"), repo.join("new.txt")).unwrap();
        std::fs::write(repo.join("b.txt"), "world\n").unwrap();
        jj_new(&tmp, &repo);
        let handling = RepoHandling::Jj { root: repo.clone() };
        let mut stderr = Vec::new();
        // Naming the rename by its *old* name still covers it (it contributes `new.txt`).
        let sel = Selection::from_cli(
            &[
                std::path::PathBuf::from("old.txt"),
                std::path::PathBuf::from("b.txt"),
            ],
            &[],
        );
        let result = generate(&handling, &sel, Some(PARENT), &repo, &mut stderr).unwrap();
        assert!(result.files.is_empty());
    }
}
