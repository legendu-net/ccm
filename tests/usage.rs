//! Stage 1 ("argument shape") usage errors — exit code 2 — driven through the real
//! binary (prd.md "Check order", stage 1; Flags section).

mod common;

use common::Fixture;
use predicates::prelude::*;

#[test]
fn include_and_exclude_together_is_a_usage_error() {
    let fx = Fixture::new();
    fx.ccm()
        .args(["--include", "a", "--exclude", "b"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty());
}

#[test]
fn gen_config_with_dry_run_is_a_usage_error() {
    let fx = Fixture::new();
    fx.ccm()
        .args(["--gen-config", "--dry-run"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty());
}

#[test]
fn gen_config_with_git_is_a_usage_error() {
    let fx = Fixture::new();
    fx.ccm()
        .args(["--gen-config", "--git"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty());
}

#[test]
fn gen_config_with_include_is_a_usage_error() {
    let fx = Fixture::new();
    fx.ccm()
        .args(["--gen-config", "--include", "a"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty());
}

#[test]
fn gen_config_combined_with_config_is_allowed_shapewise() {
    // Shape-valid; this test only pins that it is NOT rejected at stage 1 (it will
    // proceed into --gen-config itself, exercised in tests/gen_config.rs).
    let fx = Fixture::new();
    fx.ccm()
        .args(["--gen-config", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);
}

#[test]
fn unknown_flag_is_a_clap_usage_error_with_exit_code_2() {
    // Pins clap's own default error exit code, since ccm relies on it matching the
    // spec's usage-error code without any extra wiring.
    let fx = Fixture::new();
    fx.ccm().arg("--this-flag-does-not-exist").assert().code(2);
}
