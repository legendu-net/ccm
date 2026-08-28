//! Git diff generation through the real binary: exit 7 (diff command failed) and exit
//! 8 (nothing staged) (prd.md "Diff generation").

mod common;

use common::Fixture;
use predicates::prelude::*;

#[test]
fn nothing_staged_is_exit_8() {
    let fx = Fixture::new();
    fx.init_git();
    fx.write_valid_config();
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(8)
        .stderr(predicate::str::contains("Generating diff using: git"));
}

#[test]
fn unstaged_only_changes_are_invisible_and_still_exit_8() {
    let fx = Fixture::new();
    fx.init_git();
    fx.write_valid_config();
    fx.write("a.txt", "x");
    // Deliberately not staged.
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(8);
}

#[test]
fn git_not_found_on_path_is_exit_7() {
    let fx = Fixture::new();
    fx.init_git();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.stage("a.txt");

    // Override the fixture's usual PATH (which prepends an empty bin dir onto the
    // real PATH) with just the empty bin dir, so `git` itself can't be found at all.
    let mut cmd = fx.ccm();
    cmd.env("PATH", fx.bin_dir())
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(7);
}

#[test]
fn staged_changes_are_diffed() {
    let fx = Fixture::new();
    fx.init_git();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.stage("a.txt");
    // --dry-run avoids the $EDITOR flow, which this test isn't about.
    let assert = fx
        .ccm()
        .args(["--dry-run", "--config"])
        .arg(fx.config_dir())
        .assert();
    assert.code(0).stderr(predicate::str::contains(
        "Generating diff using: git --no-pager diff --no-color --staged",
    ));
}

#[test]
fn diff_progress_lines_are_on_stderr_not_stdout() {
    let fx = Fixture::new();
    fx.init_git();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.stage("a.txt");
    // Default (non-dry-run) mode, on purpose: this test is specifically about stdout
    // staying empty in the mode where stdout isn't used for the message at all. A
    // fake, no-op $EDITOR keeps the run from reaching a real interactive editor.
    fx.write_script("ed", "exit 0");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .stdout(predicate::str::is_empty());
}
