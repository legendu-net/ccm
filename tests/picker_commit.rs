//! The jj commit-command picker and the final commit invocation through the real
//! binary: exit 14 (picker cancelled), exit 16 (commit failed), and full happy paths
//! for git, jj scoped, and jj unscoped (prd.md "jj commit commands", "git commit").
//!
//! `ready`'s fake agent always returns a non-blank message, so every test here must
//! get past the message review prompt (prd.md "Message review prompt") first — a
//! leading `"\n"` (its default action, accept) before whatever jj-commit-picker input
//! follows, since the no-op editor (`"exit 0"`) would leave the message identical
//! either way.

mod common;

use common::Fixture;
use predicates::prelude::*;
use std::process::Command;

fn ready(fx: &Fixture) {
    fx.write_valid_config();
    fx.write_script("ed", "exit 0"); // no-op editor: keeps the generated message
}

#[test]
fn git_commit_happy_path_creates_a_real_commit() {
    let fx = Fixture::new();
    fx.init_git();
    ready(&fx);
    fx.write("a.txt", "hello\n");
    fx.stage("a.txt");

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("\n")
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains("Committing using: git commit")
                .and(predicate::str::contains("Committed using:")),
        );

    let log = Command::new("git")
        .args(["log", "-1", "--format=%s"])
        .current_dir(fx.repo_dir())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).trim(),
        "feat: test commit message"
    );
}

#[test]
fn git_commit_hook_rejection_is_exit_16() {
    let fx = Fixture::new();
    fx.init_git();
    ready(&fx);
    fx.write("a.txt", "hello\n");
    fx.stage("a.txt");

    // A pre-commit hook that always rejects.
    let hooks_dir = fx.repo_dir().join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let hook = hooks_dir.join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'rejected by hook' >&2\nexit 1\n").unwrap();
    let mut perms = std::fs::metadata(&hook).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&hook, perms).unwrap();

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("\n")
        .assert()
        .code(16)
        .stderr(predicate::str::contains("rejected by hook"));

    // Nothing was committed.
    let log = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(fx.repo_dir())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&log.stdout).trim().is_empty());
}

#[test]
fn jj_unscoped_picker_offers_commit_and_describe_and_commits_via_describe() {
    let fx = Fixture::new();
    fx.init_jj();
    ready(&fx);
    fx.write("a.txt", "hello\n");

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("\n1\n") // review prompt: accept; then "1) jj describe"
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains("0) jj commit")
                .and(predicate::str::contains("1) jj describe"))
                .and(predicate::str::contains("Committing using: jj describe"))
                .and(predicate::str::contains("Committed using:")),
        );

    let log = Command::new("jj")
        .args(["log", "--no-graph", "-r", "@", "-T", "description"])
        .current_dir(fx.repo_dir())
        .env("JJ_CONFIG", fx.jj_config_path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("feat: test commit message"),
        "stdout: {}",
        String::from_utf8_lossy(&log.stdout)
    );
}

#[test]
fn jj_scoped_picker_offers_commit_and_split_not_describe() {
    let fx = Fixture::new();
    fx.init_jj();
    ready(&fx);
    fx.write("a.txt", "hello\n");
    fx.write("b.txt", "world\n");

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--include", "a.txt", "--config"])
        .arg(fx.config_dir())
        .write_stdin("\n0\n") // review prompt: accept; then "0) jj commit"
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains("1) jj split")
                .and(predicate::str::contains("1) jj describe").not())
                .and(predicate::str::contains("Committing using: jj commit")),
        );
}

#[test]
fn jj_picker_cancelled_on_eof_is_exit_14_and_nothing_is_committed() {
    // True EOF at the jj-command picker (nothing follows the review prompt's own
    // accept line) is distinct from a blank line ending in Enter there (which selects
    // its default, see jj_picker_blank_input_selects_commit_as_the_default) — this pins
    // that EOF still cancels rather than silently falling back to the default. The
    // leading "\n" clears the message review prompt (accept) so this EOF is the jj
    // picker's own, not the review prompt's.
    let fx = Fixture::new();
    fx.init_jj();
    ready(&fx);
    fx.write("a.txt", "hello\n");

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("\n") // review prompt: accept; then EOF at the jj picker
        .assert()
        .code(14);

    let log = Command::new("jj")
        .args(["log", "--no-graph", "-r", "@", "-T", "description"])
        .current_dir(fx.repo_dir())
        .env("JJ_CONFIG", fx.jj_config_path())
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&log.stdout).trim().is_empty());
}

#[test]
fn jj_picker_reprompts_on_invalid_input_before_succeeding() {
    // "garbage" and "2" (out of range) are both genuinely invalid and must retry; a
    // blank line is deliberately excluded here since it now selects the default
    // (covered separately by jj_picker_blank_input_selects_commit_as_the_default) —
    // asserting the final command is `jj describe` (from the trailing "1") confirms the
    // picker actually reached and used that input rather than short-circuiting earlier.
    let fx = Fixture::new();
    fx.init_jj();
    ready(&fx);
    fx.write("a.txt", "hello\n");

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("\ngarbage\n2\n1\n") // review prompt: accept; then the jj picker
        .assert()
        .code(0)
        .stderr(predicate::str::contains("Committing using: jj describe"));
}

#[test]
fn jj_picker_blank_input_selects_commit_as_the_default() {
    let fx = Fixture::new();
    fx.init_jj();
    ready(&fx);
    fx.write("a.txt", "hello\n");

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        // review prompt: accept (its own default); then the jj picker's own blank
        // line, selecting its default in turn.
        .write_stdin("\n\n")
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains("0) jj commit  (default)")
                .and(predicate::str::contains("Committing using: jj commit")),
        );
}

#[test]
fn blank_message_never_shows_the_picker() {
    // A blank cleaned message (--dry-run's own blank-check is exercised elsewhere;
    // this drives it through the editor: the fake editor blanks the file). The picker
    // must never run for a message about to be discarded as blank. The generated
    // message itself is non-blank (from write_valid_config's fake agent), so "e"
    // explicitly selects edit at the review prompt — a blank line there would accept
    // that non-blank message outright, never reaching the blanking editor at all.
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write_script("ed", ": > \"$1\"");
    fx.write("a.txt", "hello\n");

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("e\n")
        .assert()
        .code(15)
        .stderr(predicate::str::contains("0) jj commit").not());
}
