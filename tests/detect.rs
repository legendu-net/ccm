//! Repository detection through the real binary: exit 4 (not a repo), the colocated /
//! nested-outer-repo truth table, and `--git`'s interaction with each (prd.md
//! "Repository detection").

mod common;

use common::Fixture;

#[test]
fn outside_any_repo_is_exit_4() {
    let fx = Fixture::new();
    fx.ccm().assert().code(4);
}

#[test]
fn plain_git_repo_succeeds() {
    let fx = Fixture::new();
    fx.init_git();
    fx.write_valid_config();
    fx.write("a.txt", "x");
    fx.stage("a.txt");
    // --dry-run: this test is about detection succeeding, not the $EDITOR flow (which
    // would otherwise fall back to a real interactive editor here, since $EDITOR is
    // unset and the real PATH may have nvim/vim/vi installed).
    fx.ccm()
        .args(["--dry-run", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);
}

#[test]
fn plain_jj_repo_succeeds() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write("a.txt", "x");
    fx.ccm()
        .args(["--dry-run", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);
}

#[test]
fn git_flag_outside_a_git_repo_is_a_usage_error() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.ccm().arg("--git").assert().code(2);
}

#[test]
fn git_flag_on_a_plain_git_repo_is_a_harmless_noop() {
    let fx = Fixture::new();
    fx.init_git();
    fx.write_valid_config();
    fx.write("a.txt", "x");
    fx.stage("a.txt");
    fx.ccm()
        .args(["--dry-run", "--git", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);
}

#[test]
fn colocated_repo_defaults_to_jj_handling_include_exclude_allowed() {
    let fx = Fixture::new();
    fx.init_colocated();
    fx.write("a.txt", "x");
    fx.stage("a.txt");
    // Under jj handling, --include is accepted at the usage-shape/repo-dependent
    // validation stage; later stages aren't implemented yet, so we only assert we
    // don't hit the stage-3 usage error (exit 2).
    let assert = fx.ccm().args(["--include", "a.txt"]).assert();
    assert.code(predicates::prelude::predicate::ne(2));
}

#[test]
fn colocated_repo_with_git_flag_uses_git_handling_include_rejected() {
    let fx = Fixture::new();
    fx.init_colocated();
    fx.ccm()
        .args(["--git", "--include", "a.txt"])
        .assert()
        .code(2);
}

#[test]
fn include_under_plain_git_repo_is_a_usage_error() {
    let fx = Fixture::new();
    fx.init_git();
    fx.ccm().args(["--include", "a.txt"]).assert().code(2);
}

#[test]
fn exclude_under_plain_git_repo_is_a_usage_error() {
    let fx = Fixture::new();
    fx.init_git();
    fx.ccm().args(["--exclude", "a.txt"]).assert().code(2);
}

#[test]
fn nested_git_repo_inside_outer_jj_repo_is_treated_as_git() {
    let fx = Fixture::new();
    fx.init_jj(); // outer jj root at repo_dir()
    let nested = fx.repo_dir().join("nested-git");
    std::fs::create_dir_all(&nested).unwrap();
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&nested)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success());

    // git is deeper (nested-git) -> treated as a plain git repo -> --include rejected.
    let mut cmd = fx.ccm();
    cmd.current_dir(&nested);
    cmd.args(["--include", "a.txt"]).assert().code(2);
}

#[test]
fn nested_jj_repo_inside_outer_git_repo_is_treated_as_jj() {
    let fx = Fixture::new();
    fx.init_git(); // outer git root at repo_dir()
    let nested = fx.repo_dir().join("nested-jj");
    std::fs::create_dir_all(&nested).unwrap();
    let status = std::process::Command::new("jj")
        .args(["git", "init", "--no-colocate"])
        .current_dir(&nested)
        .env("JJ_CONFIG", fx.jj_config_path())
        .status()
        .unwrap();
    assert!(status.success());

    // jj is deeper (nested-jj) -> treated as a plain jj repo -> --git is a usage error.
    let mut cmd = fx.ccm();
    cmd.current_dir(&nested);
    cmd.arg("--git").assert().code(2);
}

#[test]
fn bare_git_repo_is_not_detected() {
    let fx = Fixture::new();
    // A bare repo has no working tree: HEAD/objects/... directly, no .git subdir.
    let status = std::process::Command::new("git")
        .args(["init", "-q", "--bare"])
        .current_dir(fx.repo_dir())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success());
    fx.ccm().assert().code(4);
}
