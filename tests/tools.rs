//! `--list-tools`, `--tool <NAME>`, and `--interactive` (prd.md "Selection", "Check
//! order") through the real binary: the listing short-circuit (like `--gen-config`, it
//! needs no repo), the `--tool` override (including selecting a disabled entry, and
//! exit 6 for an unmatched name), and the interactive picker (selection, reprompting on
//! invalid input, and exit 14 on cancellation).

mod common;

use common::Fixture;
use predicates::prelude::*;

fn valid_prompts() -> &'static str {
    "default:\n  template: write a commit message\n"
}

/// A two-entry `agent_cli` config: `a` is enabled and echoes "feat: from a", `b` is
/// disabled and echoes "feat: from b". Agent CLIs (not `openai_api`) so no HTTP mock is
/// needed — matching the convention `Fixture::write_valid_config` already uses.
fn write_two_tool_config(fx: &Fixture) {
    fx.write_script("agent-a", "echo 'feat: from a'");
    fx.write_script("agent-b", "echo 'feat: from b'");
    let api = "- name: a\n  type: agent_cli\n  prompt: default\n  command: agent-a\n  model: m\n  args: []\n\
               - name: b\n  type: agent_cli\n  enabled: false\n  prompt: default\n  command: agent-b\n  model: m\n  args: []\n";
    fx.write_config(api, valid_prompts());
}

#[test]
fn list_tools_works_outside_any_repo() {
    // repo_dir() is never made a git/jj repo unless init_git/init_jj is called — this
    // proves --list-tools short-circuits before stage-2 repo detection, like
    // --gen-config.
    let fx = Fixture::new();
    write_two_tool_config(&fx);
    fx.ccm()
        .args(["--list-tools", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stdout(
            predicate::str::contains("a")
                .and(predicate::str::contains("b"))
                .and(predicate::str::contains("[enabled]"))
                .and(predicate::str::contains("[disabled]")),
        )
        .stderr(predicate::str::is_empty());
}

#[test]
fn list_tools_with_malformed_api_yaml_is_exit_5() {
    let fx = Fixture::new();
    fx.write_config("not: [valid\n", valid_prompts());
    fx.ccm()
        .args(["--list-tools", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(5);
}

#[test]
fn tool_flag_selects_a_disabled_entry() {
    let fx = Fixture::new();
    fx.init_git();
    write_two_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");

    fx.ccm()
        .args(["--tool", "b", "--dry-run", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stdout("feat: from b\n");
}

#[test]
fn tool_flag_with_unknown_name_is_exit_6() {
    let fx = Fixture::new();
    fx.init_git();
    write_two_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");

    fx.ccm()
        .args(["--tool", "no-such-tool", "--dry-run", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(6)
        .stderr(predicate::str::contains("no-such-tool"));
}

#[test]
fn interactive_picks_the_second_entry() {
    let fx = Fixture::new();
    fx.init_git();
    write_two_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");

    fx.ccm()
        .args(["--interactive", "--dry-run", "--config"])
        .arg(fx.config_dir())
        .write_stdin("1\n")
        .assert()
        .code(0)
        .stdout("feat: from b\n")
        .stderr(predicate::str::contains("Select a tool"));
}

#[test]
fn interactive_reprompts_on_invalid_input_before_succeeding() {
    let fx = Fixture::new();
    fx.init_git();
    write_two_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");

    fx.ccm()
        .args(["--interactive", "--dry-run", "--config"])
        .arg(fx.config_dir())
        .write_stdin("garbage\n\n1\n")
        .assert()
        .code(0)
        .stdout("feat: from b\n");
}

#[test]
fn interactive_cancelled_on_eof_is_exit_14() {
    let fx = Fixture::new();
    fx.init_git();
    write_two_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");

    fx.ccm()
        .args(["--interactive", "--dry-run", "--config"])
        .arg(fx.config_dir())
        .write_stdin("")
        .assert()
        .code(14)
        .stdout(predicate::str::is_empty());
}
