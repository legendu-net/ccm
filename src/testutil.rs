//! Unit-test helpers shared across modules. Each helper runs `jj` in `repo` with the
//! throwaway user config the per-module `jj_repo()` fixtures write to
//! `<tmp>/jj.toml`.

use crate::vcs::argv::Revision;
use std::path::Path;

/// `--revision @-`, as `diff::check_revision` returns it.
pub(crate) const PARENT: Revision<'static> = Revision {
    revset: "@-",
    is_working_copy: false,
};

/// `jj new` in `repo`, so the changes made so far become `@-` and later writes land
/// in a fresh `@`.
pub(crate) fn jj_new(tmp: &tempfile::TempDir, repo: &Path) {
    run_jj(tmp, repo, &["new"]);
}

/// `jj log --no-graph -r <revision> -T <template>`'s stdout.
pub(crate) fn jj_log(
    tmp: &tempfile::TempDir,
    repo: &Path,
    revision: &str,
    template: &str,
) -> String {
    run_jj(
        tmp,
        repo,
        &["log", "--no-graph", "-r", revision, "-T", template],
    )
}

/// Runs `jj <args>` in `repo`, asserting it succeeds, and returns its stdout.
pub(crate) fn run_jj(tmp: &tempfile::TempDir, repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("jj")
        .args(args)
        .current_dir(repo)
        .env("JJ_CONFIG", tmp.path().join("jj.toml"))
        .output()
        .unwrap();
    assert!(out.status.success(), "jj {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}
