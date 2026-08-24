//! The check-order pipeline is a fixed sequence of stages where the first failure
//! wins (prd.md "Check order"). This file constructs *simultaneous* failures and
//! asserts which stage's exit code actually wins, pinning the normative order itself
//! rather than any single stage in isolation.

mod common;

use common::Fixture;
use predicates::prelude::*;

#[test]
fn no_repo_beats_missing_config_and_include_the_specs_own_worked_example() {
    // prd.md's own worked example: no repo, api.yaml missing, --include given -> 4.
    let fx = Fixture::new();
    fx.ccm()
        .args(["--include", "x", "--config"])
        .arg(fx.root_path().join("no-such-config"))
        .assert()
        .code(4);
}

#[test]
fn argument_shape_beats_repo_detection() {
    // --include + --exclude together (stage 1) vs. no repo at all (stage 2): stage 1
    // wins even outside any repo.
    let fx = Fixture::new();
    fx.ccm()
        .args(["--include", "a", "--exclude", "b"])
        .assert()
        .code(2);
}

#[test]
fn gen_config_with_dry_run_is_rejected_before_touching_gen_config_at_all() {
    let fx = Fixture::new();
    fx.ccm()
        .args(["--gen-config", "--dry-run"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty());
}

#[test]
fn gen_config_outside_any_repo_still_succeeds() {
    // --gen-config short-circuits every later stage, including repo detection.
    let fx = Fixture::new();
    fx.ccm()
        .args(["--gen-config", "--config"])
        .arg(fx.root_path().join("cfg"))
        .assert()
        .code(0);
}

#[test]
fn repo_dependent_usage_beats_missing_config_git_repo() {
    // --include under a plain git repo (stage 3) vs. missing api.yaml (stage 4):
    // stage 3 wins even though the config directory doesn't exist either.
    let fx = Fixture::new();
    fx.init_git();
    fx.ccm()
        .args(["--include", "x", "--config"])
        .arg(fx.root_path().join("no-such-config"))
        .assert()
        .code(2);
}

#[test]
fn repo_dependent_usage_beats_missing_config_jj_repo_with_git_flag() {
    // --git outside a git repository (stage 3) vs. missing api.yaml (stage 4).
    let fx = Fixture::new();
    fx.init_jj();
    fx.ccm()
        .args(["--git", "--config"])
        .arg(fx.root_path().join("no-such-config"))
        .assert()
        .code(2);
}

#[test]
fn config_load_beats_selection() {
    // Malformed api.yaml (stage 4) vs. every entry disabled — can't even get to
    // stage 5 without a config that parses.
    let fx = Fixture::new();
    fx.init_git();
    fx.write_config("not: [valid\n", "default:\n  template: x\n");
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(5);
}

#[test]
fn selection_beats_diff_generation() {
    // Every entry disabled (stage 5) fails fast, before diff generation (stage 6) --
    // even with nothing staged, which would otherwise be exit 8.
    let fx = Fixture::new();
    fx.init_git();
    fx.write_config(
        "- name: a\n  type: openai_api\n  enabled: false\n  prompt: default\n  base_url: http://x\n  model: m\n  api_key:\n    env: K\n",
        "default:\n  template: x\n",
    );
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(6);
}

#[test]
fn scope_resolution_beats_api_key_resolution() {
    // An unmatched --include path (part of stage 6) vs. an unset api_key env var
    // (stage 7): the usage error is caught before the diff is even generated, let
    // alone before the tool/API is called.
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_config(
        "- name: a\n  type: openai_api\n  prompt: default\n  base_url: http://127.0.0.1:1\n  model: m\n  api_key:\n    env: CCM_CHECK_ORDER_UNSET\n",
        "default:\n  template: x\n",
    );
    fx.write("a.txt", "hello\n");
    fx.ccm()
        .args(["--include", "typo.txt", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(2);
}

#[test]
fn blank_message_never_reaches_the_jj_picker() {
    // A blank cleaned message (stage 8's empty-message check) must abort with exit 15
    // before the jj commit-command picker ever runs — the picker is never shown for a
    // message that's about to be discarded.
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write_script("ed", ": > \"$1\""); // saves a blank file
    fx.write("a.txt", "hello\n");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("0\n")
        .assert()
        .code(15)
        .stderr(
            predicate::str::contains("0) jj commit")
                .not()
                .and(predicate::str::contains("1) jj describe").not()),
        );
}
