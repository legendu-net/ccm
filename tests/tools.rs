//! `--list-tools`, `--tool <NAME>`, and default mode's tool picker (prd.md "Selection",
//! "Check order") through the real binary: the listing short-circuit (like
//! `--gen-config`, it needs no repo), the `--tool` override (including selecting a
//! disabled entry, and exit 6 for an unmatched name), the `--tool ''` force-picker
//! sentinel, and the tool picker default mode shows whenever the first-enabled rule
//! can't resolve on its own (selection, reprompting on invalid input, and exit 14 on
//! cancellation).
//!
//! Every generated message here is non-blank, so once tool selection is settled, an
//! extra `"e\n"` is needed to get past the message review prompt (prd.md "Message
//! review prompt") and actually reach the (fake, capturing) `$EDITOR` these tests
//! inspect — a blank line there would accept the message outright and skip `$EDITOR`.

mod common;

use common::Fixture;
use predicates::prelude::*;

fn valid_prompts() -> &'static str {
    "default:\n  template: write a commit message\n"
}

/// A two-entry `agent_cli` config: `a` is enabled and echoes "feat: from a", `b` is
/// disabled and echoes "feat: from b". Agent CLIs (not `openai_api`) so no HTTP mock is
/// needed — matching the convention `Fixture::write_valid_config` already uses. Exactly
/// one entry enabled, so default mode selects "a" silently — used by tests pinning that
/// no-prompt behavior, and by `--tool`/`--list-tools` tests where the picker is
/// irrelevant.
fn write_two_tool_config(fx: &Fixture) {
    fx.write_script("agent-a", "echo 'feat: from a'");
    fx.write_script("agent-b", "echo 'feat: from b'");
    let api = "- name: a\n  type: agent_cli\n  prompt: default\n  command: agent-a\n  model: m\n  args: []\n\
               - name: b\n  type: agent_cli\n  enabled: false\n  prompt: default\n  command: agent-b\n  model: m\n  args: []\n";
    fx.write_config(api, valid_prompts());
}

/// Same shape as [`write_two_tool_config`], but both entries are `enabled: true` — the
/// choice is genuinely ambiguous, so default mode's tool picker triggers.
fn write_two_enabled_tool_config(fx: &Fixture) {
    fx.write_script("agent-a", "echo 'feat: from a'");
    fx.write_script("agent-b", "echo 'feat: from b'");
    let api = "- name: a\n  type: agent_cli\n  prompt: default\n  command: agent-a\n  model: m\n  args: []\n\
               - name: b\n  type: agent_cli\n  prompt: default\n  command: agent-b\n  model: m\n  args: []\n";
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
        .stdout("feat: from b"); // trimmed of the "echo"-appended trailing newline
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
fn empty_tool_flag_forces_the_picker_onto_a_disabled_entry_under_dry_run() {
    // `--tool ''` is the force-picker sentinel: "a" is the sole enabled entry (which
    // `--dry-run` would otherwise take silently, with no prompt at all — see
    // tool_flag_selects_a_disabled_entry's sibling, dry_run's own no-tool-flag tests in
    // tests/stdio.rs), but `--tool ''` must still show the picker and let the user
    // reach the disabled entry "b" without touching api.yaml.
    let fx = Fixture::new();
    fx.init_git();
    write_two_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");

    fx.ccm()
        .args(["--tool", "", "--dry-run", "--config"])
        .arg(fx.config_dir())
        .write_stdin("1\n")
        .assert()
        .code(0)
        .stdout("feat: from b") // trimmed of the "echo"-appended trailing newline
        .stderr(predicate::str::contains("Enter an index"));
}

/// A fake `$EDITOR` that copies the temp file's pre-populated content (the generated
/// message the tool picker's choice produced, plus the `#CCM:` comment) to `marker`
/// before overwriting the temp file with a fixed, always-committable message — so the
/// run still reaches a real `git commit` (exit 0) while `marker` pins which tool
/// actually ran, matching the pattern `tests/editor.rs`'s
/// `the_temp_file_is_pre_populated_with_the_generated_message_and_ccm_comment` uses.
fn install_capturing_editor(fx: &Fixture, marker: &std::path::Path) {
    fx.write_script(
        "ed",
        &format!(
            "cp \"$1\" {}\nprintf 'feat: edited\\n' > \"$1\"",
            marker.display()
        ),
    );
}

#[test]
fn no_tool_flag_picker_selects_the_second_entry() {
    // Both entries enabled, so default mode's picker triggers; picking index 1 ("b")
    // must be what generates the commit message.
    let fx = Fixture::new();
    fx.init_git();
    write_two_enabled_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");
    let marker = fx.tmp_dir().join("captured.txt");
    install_capturing_editor(&fx, &marker);

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("1\ne\n")
        .assert()
        .code(0)
        .stderr(predicate::str::contains("Enter an index"));
    let captured = std::fs::read_to_string(&marker).unwrap();
    assert!(captured.starts_with("feat: from b"));
}

#[test]
fn empty_tool_flag_forces_the_picker_onto_a_disabled_entry_in_default_mode() {
    // The motivating scenario: only "a" is enabled (which default mode would
    // otherwise take silently, see single_enabled_entry_selects_silently_in_default_mode),
    // but `--tool ''` must still show the picker and let picking index 1 reach the
    // disabled entry "b" — proven by the actual generated message, not just an exit
    // code, since a weaker assertion couldn't tell "picker ran and picked b" apart from
    // "picker ran and picked a" or any other outcome that still reaches the editor.
    let fx = Fixture::new();
    fx.init_git();
    write_two_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");
    let marker = fx.tmp_dir().join("captured.txt");
    install_capturing_editor(&fx, &marker);

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--tool", "", "--config"])
        .arg(fx.config_dir())
        .write_stdin("1\ne\n")
        .assert()
        .code(0)
        .stderr(predicate::str::contains("Enter an index"));
    let captured = std::fs::read_to_string(&marker).unwrap();
    assert!(captured.starts_with("feat: from b"));
}

#[test]
fn picker_reprompts_on_invalid_input_before_succeeding() {
    // "garbage" and "99" (out of range) are both genuinely invalid and must retry; a
    // blank line is deliberately excluded here since it now selects the default entry
    // "a" (covered separately by picker_blank_input_selects_the_default_tool).
    let fx = Fixture::new();
    fx.init_git();
    write_two_enabled_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");
    let marker = fx.tmp_dir().join("captured.txt");
    install_capturing_editor(&fx, &marker);

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("garbage\n99\n1\ne\n")
        .assert()
        .code(0);
    let captured = std::fs::read_to_string(&marker).unwrap();
    assert!(captured.starts_with("feat: from b"));
}

#[test]
fn picker_blank_input_selects_the_default_tool() {
    // "a" is the first-enabled/default entry; a blank line at the prompt selects it
    // without the user typing "0". The prompt states the default index directly
    // ("Enter an index [default 0]: ") rather than marking the entry's own line, so
    // that's what this test looks for.
    let fx = Fixture::new();
    fx.init_git();
    write_two_enabled_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");
    let marker = fx.tmp_dir().join("captured.txt");
    install_capturing_editor(&fx, &marker);

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("\ne\n")
        .assert()
        .code(0)
        .stderr(predicate::str::contains("Enter an index [default 0]"));
    let captured = std::fs::read_to_string(&marker).unwrap();
    assert!(captured.starts_with("feat: from a"));
}

#[test]
fn picker_blank_input_without_a_default_still_retries() {
    // Every entry is disabled here (zero enabled, so the picker still triggers), and
    // there's no default — a blank line must still retry, and only an explicit index
    // selects an entry.
    let fx = Fixture::new();
    fx.init_git();
    fx.write_script("agent-a", "echo 'feat: from a'");
    fx.write_config(
        "- name: a\n  type: agent_cli\n  enabled: false\n  prompt: default\n  command: agent-a\n  model: m\n  args: []\n",
        valid_prompts(),
    );
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");
    let marker = fx.tmp_dir().join("captured.txt");
    install_capturing_editor(&fx, &marker);

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("\n0\ne\n")
        .assert()
        .code(0);
    let captured = std::fs::read_to_string(&marker).unwrap();
    assert!(captured.starts_with("feat: from a"));
}

#[test]
fn picker_cancelled_on_eof_is_exit_14() {
    // True EOF (write_stdin("") closes stdin with zero bytes) is distinct from a blank
    // line ending in Enter (which now selects the default, see
    // picker_blank_input_selects_the_default_tool) — this pins that EOF still cancels
    // rather than silently falling back to the default.
    let fx = Fixture::new();
    fx.init_git();
    write_two_enabled_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");

    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("")
        .assert()
        .code(14)
        .stdout(predicate::str::is_empty());
}

#[test]
fn single_enabled_entry_selects_silently_in_default_mode() {
    // Exactly one entry enabled ("a"; "b" disabled): default mode must not show the
    // tool picker at all, so its stdin's first line ("e\n") is read by the message
    // review prompt instead, selecting edit and reaching the (fake) editor to commit
    // using "a"'s message.
    let fx = Fixture::new();
    fx.init_git();
    write_two_tool_config(&fx);
    fx.write("f.txt", "hello\n");
    fx.stage("f.txt");
    let marker = fx.tmp_dir().join("captured.txt");
    install_capturing_editor(&fx, &marker);

    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .write_stdin("e\n")
        .assert()
        .code(0)
        .stderr(predicate::str::contains("Select a tool").not());
    let captured = std::fs::read_to_string(&marker).unwrap();
    assert!(captured.starts_with("feat: from a"));
}
