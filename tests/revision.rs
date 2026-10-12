//! `--revision`/`-r` through the real binary (prd.md "Flags", "Diff generation", "jj
//! commit commands"): diffing a revision other than the working copy, the
//! exactly-one-revision check, and describing/splitting that revision without the jj
//! commit-command picker.

mod common;

use common::Fixture;
use predicates::prelude::*;
use std::process::Command;

/// A jj repo whose `@-` changed `a.txt` and whose working copy `@` changed `b.txt`.
fn parent_and_working_copy() -> Fixture {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.jj_new("wip");
    fx.write("b.txt", "world\n");
    fx
}

fn description(fx: &Fixture, revision: &str) -> String {
    let log = Command::new("jj")
        .args(["log", "--no-graph", "-r", revision, "-T", "description"])
        .current_dir(fx.repo_dir())
        .env("JJ_CONFIG", fx.jj_config_path())
        .output()
        .unwrap();
    assert!(log.status.success());
    String::from_utf8_lossy(&log.stdout).trim().to_string()
}

#[test]
fn dry_run_diffs_the_given_revision() {
    let fx = parent_and_working_copy();
    fx.ccm()
        .args(["--dry-run", "-r", "@-", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stderr(predicate::str::contains(
            "Generating diff using: jj --no-pager --ignore-working-copy diff '--color=never' -r @-\n",
        ));
}

#[test]
fn include_is_matched_against_the_given_revision() {
    let fx = parent_and_working_copy();
    fx.ccm()
        .args([
            "--dry-run",
            "--revision",
            "@-",
            "--include",
            "a.txt",
            "--config",
        ])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stderr(predicate::str::contains(
            "Generating diff using: jj --no-pager --ignore-working-copy diff '--color=never' -r @- a.txt\n",
        ));

    // b.txt only changed in the working copy, not in @-.
    fx.ccm()
        .args(["--dry-run", "-r", "@-", "--include", "b.txt", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "b.txt: does not match any changed file",
        ));
}

#[test]
fn a_revset_naming_several_revisions_is_a_usage_error() {
    let fx = parent_and_working_copy();
    fx.ccm()
        .args(["--dry-run", "-r", "all()", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--revision all(): must resolve to exactly one revision",
        ));
}

#[test]
fn an_unknown_revision_is_exit_7() {
    let fx = parent_and_working_copy();
    fx.ccm()
        .args(["--dry-run", "-r", "no_such_bookmark_xyz", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(7)
        .stderr(predicate::str::contains("failed to resolve revision"));
}

#[test]
fn an_unknown_revision_fails_before_config_load() {
    // No config written at all: the stage-3 revision check (exit 7) must win over
    // stage 4 (exit 5).
    let fx = Fixture::new();
    fx.init_jj();
    fx.ccm()
        .args(["--dry-run", "-r", "no_such_bookmark_xyz", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(7)
        .stderr(predicate::str::contains("failed to resolve revision"));
}

#[test]
fn revision_under_git_handling_is_a_usage_error_before_config_load() {
    // No config written at all: stage 3 (exit 2) must win over stage 4 (exit 5).
    let fx = Fixture::new();
    fx.init_git();
    fx.ccm()
        .args(["-r", "HEAD", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--revision is only supported for jj repositories",
        ));
}

#[test]
fn default_mode_describes_the_given_revision_without_the_picker() {
    let fx = parent_and_working_copy();
    fx.write_script("ed", "exit 0");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["-r", "@-", "--config"])
        .arg(fx.config_dir())
        .write_stdin("\n") // review prompt: accept; no jj commit-command picker follows
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains("Which jj command to use?")
                .not()
                .and(predicate::str::contains(
                    "Committed using: jj describe -m 'feat: test commit message' -r @-",
                )),
        );
    assert_eq!(description(&fx, "@-"), "feat: test commit message");
    assert_eq!(description(&fx, "@"), "wip");
}

#[test]
fn default_mode_with_include_splits_the_given_revision_without_the_picker() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write_script("ed", "exit 0");
    fx.write("a.txt", "hello\n");
    fx.write("b.txt", "world\n");
    fx.jj_new("wip");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["-r", "@-", "--include", "a.txt", "--config"])
        .arg(fx.config_dir())
        .write_stdin("\n")
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains("Which jj command to use?")
                .not()
                .and(predicate::str::contains(
                    "Committed using: jj split -m 'feat: test commit message' -r @- -- a.txt",
                )),
        );
    // The selected half keeps the message; the remainder sits between it and @.
    assert_eq!(description(&fx, "@--"), "feat: test commit message");
    assert_eq!(description(&fx, "@"), "wip");
}

#[test]
fn exclude_is_matched_against_the_given_revision() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.write("b.txt", "world\n");
    fx.jj_new("wip");
    fx.write("c.txt", "working copy only\n");
    fx.ccm()
        .args(["--dry-run", "-r", "@-", "--exclude", "a.txt", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stderr(predicate::str::contains(
            "Generating diff using: jj --no-pager --ignore-working-copy diff '--color=never' -r @- b.txt\n",
        ));
}

#[test]
fn an_empty_revision_is_exit_8() {
    let fx = parent_and_working_copy();
    // `@-` is then a single (mutable) revision with no changes of its own.
    fx.jj_new("empty");
    fx.jj_new("top");
    fx.ccm()
        .args(["--dry-run", "-r", "@-", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(8);
}

#[test]
fn an_immutable_revision_is_exit_2() {
    let fx = parent_and_working_copy();
    fx.ccm()
        .args(["--dry-run", "-r", "root()", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--revision root(): revision is immutable",
        ));
}

#[test]
fn default_mode_with_a_scope_covering_the_whole_revision_describes_instead_of_splitting() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write_script("ed", "exit 0");
    fx.write("a.txt", "hello\n");
    fx.write("b.txt", "world\n");
    fx.jj_new("wip");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["-r", "@-", "--include", "a.txt", "b.txt", "--config"])
        .arg(fx.config_dir())
        .write_stdin("\n")
        .assert()
        .code(0)
        .stderr(predicate::str::contains(
            "Committed using: jj describe -m 'feat: test commit message' -r @-",
        ));
    // No empty leftover revision: @- is still the one revision holding a.txt and b.txt.
    assert_eq!(description(&fx, "@-"), "feat: test commit message");
    assert_eq!(description(&fx, "@"), "wip");
    assert_eq!(description(&fx, "@--"), "");
}

fn operation_count(fx: &Fixture) -> usize {
    let log = Command::new("jj")
        .args([
            "--ignore-working-copy",
            "op",
            "log",
            "--no-graph",
            "-T",
            "id ++ \"\\n\"",
        ])
        .current_dir(fx.repo_dir())
        .env("JJ_CONFIG", fx.jj_config_path())
        .output()
        .unwrap();
    assert!(log.status.success());
    String::from_utf8_lossy(&log.stdout).lines().count()
}

#[test]
fn a_non_working_copy_revision_does_not_snapshot_the_working_copy() {
    let fx = parent_and_working_copy();
    let before = operation_count(&fx);
    fx.ccm()
        .args(["--dry-run", "-r", "@-", "--include", "a.txt", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);
    fx.ccm()
        .args(["--dry-run", "-r", "@-", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);
    assert_eq!(operation_count(&fx), before);
}

#[test]
fn the_working_copy_revision_still_snapshots_the_working_copy() {
    let fx = parent_and_working_copy();
    let before = operation_count(&fx);
    fx.ccm()
        .args(["--dry-run", "-r", "@", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stderr(predicate::str::contains(
            "Generating diff using: jj --no-pager diff '--color=never' -r @\n",
        ));
    // `b.txt` was written after the last snapshot, so diffing `@` records one.
    assert!(operation_count(&fx) > before);
}
