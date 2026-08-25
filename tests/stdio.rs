//! Stdout/stderr separation (prd.md "Progress logging"): stdout is reserved solely for
//! `--dry-run`'s message and `--gen-config`'s report; every progress line and the jj
//! picker's prompts go to stderr, in all modes, on both success and failure.

mod common;

use common::Fixture;
use predicates::prelude::*;

fn ready(fx: &Fixture) {
    fx.init_git();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.stage("a.txt");
}

#[test]
fn dry_run_stdout_is_byte_exact_with_no_added_newline() {
    let fx = Fixture::new();
    ready(&fx);
    fx.ccm()
        .args(["--dry-run", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stdout("feat: test commit message\n"); // the fake agent_cli's own exact output
}

#[test]
fn dry_run_never_prints_progress_lines_to_stdout() {
    let fx = Fixture::new();
    ready(&fx);
    fx.ccm()
        .args(["--dry-run", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stdout(predicate::str::contains("Generating").not())
        .stdout(predicate::str::contains("Commit message").not());
}

#[test]
fn default_mode_stdout_stays_empty_through_a_full_successful_run() {
    let fx = Fixture::new();
    ready(&fx);
    fx.write_script("ed", "exit 0");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stdout(predicate::str::is_empty());
}

#[test]
fn gen_config_output_is_on_stdout_and_stderr_stays_empty() {
    let fx = Fixture::new();
    fx.ccm()
        .args(["--gen-config", "--config"])
        .arg(fx.root_path().join("cfg"))
        .assert()
        .code(0)
        .stdout(predicate::str::contains("created"))
        .stderr(predicate::str::is_empty());
}

#[test]
fn gen_config_failure_message_is_on_stderr_stdout_stays_empty() {
    let fx = Fixture::new();
    let target = fx.root_path().join("cfg");
    std::fs::write(&target, "not a directory").unwrap();
    fx.ccm()
        .args(["--gen-config", "--config"])
        .arg(&target)
        .assert()
        .code(3)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty().not());
}

/// Every error path this suite covers elsewhere leaves stdout empty; a representative
/// sample across several different stages, gathered in one place.
#[test]
fn error_paths_across_stages_all_leave_stdout_empty() {
    // Stage 1: usage error.
    let fx = Fixture::new();
    fx.ccm()
        .args(["--include", "a", "--exclude", "b"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty());

    // Stage 2: not a repo.
    let fx = Fixture::new();
    fx.ccm().assert().code(4).stdout(predicate::str::is_empty());

    // Stage 4: config error.
    let fx = Fixture::new();
    fx.init_git();
    fx.write_config("not: [valid\n", "default:\n  template: x\n");
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(5)
        .stdout(predicate::str::is_empty());

    // Stage 6: nothing to diff.
    let fx = Fixture::new();
    fx.init_git();
    fx.write_valid_config();
    fx.ccm()
        .args(["--dry-run", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(8)
        .stdout(predicate::str::is_empty());

    // Stage 8: editor unavailable.
    let fx = Fixture::new();
    ready(&fx);
    fx.symlink_git_into_bin_dir();
    fx.ccm()
        .env("PATH", fx.bin_dir())
        .env("EDITOR", "")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(12)
        .stdout(predicate::str::is_empty());
}

#[test]
fn list_tools_output_is_on_stdout_and_stderr_stays_empty() {
    let fx = Fixture::new();
    fx.write_valid_config();
    fx.ccm()
        .args(["--list-tools", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stdout(predicate::str::contains('a'))
        .stderr(predicate::str::is_empty());
}

#[test]
fn interactive_tool_prompt_is_on_stderr_and_stdout_holds_only_the_message() {
    let fx = Fixture::new();
    fx.init_git();
    fx.write_valid_config(); // a single agent_cli entry named "a"
    fx.write("a.txt", "hello\n");
    fx.stage("a.txt");
    fx.ccm()
        .args(["--interactive", "--dry-run", "--config"])
        .arg(fx.config_dir())
        .write_stdin("0\n")
        .assert()
        .code(0)
        .stdout("feat: test commit message\n")
        .stderr(predicate::str::contains("Select a tool"));
}

#[test]
fn full_jj_include_run_progress_lines_appear_on_stderr_in_order() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write_script("ed", "exit 0");
    fx.write("a.txt", "hello\n");
    fx.write("b.txt", "world\n");

    let assert = fx
        .ccm()
        .env("EDITOR", "ed")
        .args(["--include", "a.txt", "--config"])
        .arg(fx.config_dir())
        .write_stdin("0\n")
        .assert()
        .code(0);

    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    let expected_order = [
        "Enumerating working-copy files using:",
        "Working-copy files enumerated by:",
        "Generating diff using:",
        "Diff generated by:",
        "Generating commit message using",
        "Commit message generated by",
        "0) jj commit",
        "Committing using:",
        "Committed using:",
    ];
    let mut last_pos = 0;
    for marker in expected_order {
        let pos = stderr[last_pos..].find(marker).unwrap_or_else(|| {
            panic!("expected {marker:?} after position {last_pos} in:\n{stderr}")
        });
        last_pos += pos + marker.len();
    }
}
