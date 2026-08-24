//! The final stage of the check-order pipeline: `git commit` / the jj commit-command
//! picker plus `jj commit`|`describe`|`split` (prd.md "git commit", "jj commit
//! commands"). Like `diff.rs`, this spans more than one final exit code (14, the
//! picker being cancelled; 16, the commit invocation itself failing) within one
//! spec-numbered stage, so it returns [`CcmError`] directly.

use crate::error::{CcmError, CommitError, PickerCancelled};
use crate::picker;
use crate::progress;
use crate::repo::RepoHandling;
use crate::vcs::argv;
use crate::vcs::exec;
use std::io::BufRead;
use std::path::Path;

/// Commits `message` (already cleaned and confirmed non-blank by the caller). For a
/// jj repository, `files` (the same scope `diff::generate` resolved) is threaded
/// through to the picked jj command unchanged, so the commit matches what `message`
/// describes; a non-empty `files` also determines whether `jj split` (scoped) or `jj
/// describe` (unscoped) is offered.
///
/// # Errors
/// [`crate::error::PickerCancelled`] (exit 14, jj only) or [`CommitError`] (exit 16).
pub fn commit(
    handling: &RepoHandling,
    message: &str,
    files: &[String],
    cwd: &Path,
    stdin: &mut dyn BufRead,
    stderr: &mut dyn std::io::Write,
) -> Result<(), CcmError> {
    match handling {
        RepoHandling::Git { .. } => commit_git(message, cwd, stderr),
        RepoHandling::Jj { .. } => commit_jj(message, files, cwd, stdin, stderr),
    }
}

fn commit_git(message: &str, cwd: &Path, stderr: &mut dyn std::io::Write) -> Result<(), CcmError> {
    let args = argv::git_commit_args(message);
    let command = argv::render_command("git", &args);
    let _ = progress::committing_using(stderr, &command);

    let captured = exec::run_simple("git", args, cwd).map_err(CommitError)?;
    if !captured.success {
        return Err(CommitError(exec::owned_utf8_lossy(captured.stderr)).into());
    }

    let _ = progress::committed_using(stderr, &command);
    Ok(())
}

fn commit_jj(
    message: &str,
    files: &[String],
    cwd: &Path,
    stdin: &mut dyn BufRead,
    stderr: &mut dyn std::io::Write,
) -> Result<(), CcmError> {
    // A scope was given iff `files` is non-empty by the time commit() is called: the
    // "exclude everything" empty-scope case was already short-circuited to exit 8 in
    // diff::generate, and `--include` never resolves to an empty list (every pattern
    // is required to have matched at least one line).
    let scoped = !files.is_empty();
    let choices = picker::choices(scoped);
    let picked = picker::prompt(stdin, stderr, &choices).map_err(|err| {
        if matches!(err, picker::PickerError::Cancelled) {
            CcmError::from(PickerCancelled)
        } else {
            CcmError::Unexpected(err.to_string())
        }
    })?;

    let args = argv::jj_commit_command_args(picked, message, files);
    let command = argv::render_command("jj", &args);
    let _ = progress::committing_using(stderr, &command);

    let captured = exec::run_simple("jj", args, cwd).map_err(CommitError)?;
    if !captured.success {
        return Err(CommitError(exec::owned_utf8_lossy(captured.stderr)).into());
    }

    let _ = progress::committed_using(stderr, &command);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn git_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
        ] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(tmp.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .status()
                .unwrap();
        }
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
    fn git_commit_succeeds_with_a_staged_change() {
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
        let mut stdin = Cursor::new(Vec::new());
        let mut stderr = Vec::new();
        commit(
            &handling,
            "feat: add a",
            &[],
            repo.path(),
            &mut stdin,
            &mut stderr,
        )
        .unwrap();

        let log = std::process::Command::new("git")
            .args(["log", "-1", "--format=%s"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "feat: add a");
    }

    #[test]
    fn git_commit_failure_is_exit_16() {
        // No staged changes at all -> `git commit` itself fails.
        let repo = git_repo();
        let handling = RepoHandling::Git {
            root: repo.path().to_path_buf(),
        };
        let mut stdin = Cursor::new(Vec::new());
        let mut stderr = Vec::new();
        let err = commit(
            &handling,
            "feat: x",
            &[],
            repo.path(),
            &mut stdin,
            &mut stderr,
        )
        .unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 16);
    }

    #[test]
    fn jj_commit_picker_offers_describe_when_unscoped_and_commits() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let mut stdin = Cursor::new(b"0\n".to_vec());
        let mut stderr = Vec::new();
        commit(&handling, "feat: x", &[], &repo, &mut stdin, &mut stderr).unwrap();
        let logged = String::from_utf8(stderr).unwrap();
        assert!(logged.contains("0) jj commit"));
        assert!(logged.contains("1) jj describe"));
    }

    #[test]
    fn jj_picker_offers_split_when_scoped() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let mut stdin = Cursor::new(b"0\n".to_vec());
        let mut stderr = Vec::new();
        commit(
            &handling,
            "feat: x",
            &["a.txt".to_string()],
            &repo,
            &mut stdin,
            &mut stderr,
        )
        .unwrap();
        let logged = String::from_utf8(stderr).unwrap();
        assert!(logged.contains("1) jj split"));
    }

    #[test]
    fn jj_picker_cancelled_on_eof_is_exit_14() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.txt"), "hello\n").unwrap();
        let handling = RepoHandling::Jj { root: repo.clone() };
        let mut stdin = Cursor::new(Vec::new());
        let mut stderr = Vec::new();
        let err = commit(&handling, "feat: x", &[], &repo, &mut stdin, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
    }

    #[test]
    fn jj_commit_command_failure_is_exit_16() {
        // A directory that was never `jj git init`'d at all -> `jj commit` itself
        // fails outright, regardless of the picker's selection.
        let tmp = tempfile::tempdir().unwrap();
        let handling = RepoHandling::Jj {
            root: tmp.path().to_path_buf(),
        };
        let mut stdin = Cursor::new(b"0\n".to_vec());
        let mut stderr = Vec::new();
        let err = commit(
            &handling,
            "feat: x",
            &[],
            tmp.path(),
            &mut stdin,
            &mut stderr,
        )
        .unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 16);
    }
}
