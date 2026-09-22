//! Stage 4 (config load & validation, exit 5) and stage 5 (tool/API selection, exit 6)
//! through the real binary (prd.md "Check order").

mod common;

use common::Fixture;
use predicates::prelude::*;

fn valid_prompts() -> &'static str {
    "default:\n  template: write a commit message\n"
}

fn openai_entry(enabled: bool) -> String {
    format!(
        "- name: a\n  type: openai_api\n  enabled: {enabled}\n  prompt: default\n  base_url: http://127.0.0.1:1\n  model: m\n  api_key:\n    env: CCM_TEST_KEY\n"
    )
}

#[test]
fn missing_config_dir_is_exit_5() {
    let fx = Fixture::new();
    fx.init_git();
    fx.ccm()
        .args(["--config"])
        .arg(fx.root_path().join("does-not-exist"))
        .assert()
        .code(5);
}

#[test]
fn malformed_api_yaml_is_exit_5() {
    let fx = Fixture::new();
    fx.init_git();
    fx.write_config("not: [valid\n", valid_prompts());
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(5);
}

#[test]
fn empty_api_yaml_list_is_exit_5() {
    let fx = Fixture::new();
    fx.init_git();
    fx.write_config("[]\n", valid_prompts());
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(5);
}

#[test]
fn duplicate_entry_name_is_exit_5() {
    let fx = Fixture::new();
    fx.init_git();
    let api = format!("{}{}", openai_entry(true), openai_entry(true));
    fx.write_config(&api, valid_prompts());
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(5);
}

#[test]
fn unknown_prompt_reference_is_exit_5() {
    let fx = Fixture::new();
    fx.init_git();
    fx.write_config(&openai_entry(true), "other:\n  template: x\n");
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(5);
}

#[test]
fn agent_cli_args_containing_prompt_placeholder_is_exit_5_with_guidance() {
    // prd.md, `type: agent_cli` fields under Configuration: `{{prompt}}` in `args` is
    // obsolete now that the prompt template is joined with the diff and sent on stdin
    // instead (see src/backend/agent_cli.rs and src/prompt.rs).
    let fx = Fixture::new();
    fx.init_git();
    let api = "- name: a\n  type: agent_cli\n  prompt: default\n  command: agent\n  model: m\n  args: [\"-p\", \"{{prompt}}\"]\n";
    fx.write_config(api, valid_prompts());
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(5)
        .stderr(predicate::str::contains("{{prompt}}"))
        .stderr(predicate::str::contains("no longer supported"));
}

#[test]
fn unknown_yaml_key_is_exit_5() {
    let fx = Fixture::new();
    fx.init_git();
    let api = "- name: a\n  type: openai_api\n  prompt: default\n  base_url: http://x\n  model: m\n  api_key:\n    env: K\n  temprature: 0.2\n";
    fx.write_config(api, valid_prompts());
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(5);
}

#[test]
fn all_entries_disabled_is_exit_6() {
    // `--dry-run` never prompts (see prd.md "Interactive terminal requirement"), so it
    // still applies the plain first-enabled rule and fails outright here; default mode
    // would instead show the tool picker (see tests/tools.rs).
    let fx = Fixture::new();
    fx.init_git();
    fx.write_config(&openai_entry(false), valid_prompts());
    fx.ccm()
        .args(["--dry-run", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(6);
}

#[test]
fn missing_config_beats_selection_since_it_runs_first() {
    // Stage 4 (config load) precedes stage 5 (selection) — nothing to select from a
    // config directory that isn't even readable.
    let fx = Fixture::new();
    fx.init_git();
    fx.ccm()
        .args(["--config"])
        .arg(fx.root_path().join("still-missing"))
        .assert()
        .code(5);
}

#[test]
fn shipped_gen_config_assets_pass_config_load_and_selection() {
    // A round trip through the real binary: --gen-config writes the example files,
    // then a plain run against that same directory must get past stages 4 and 5 (it
    // will fail later, at diff generation, which isn't implemented as of this phase —
    // asserting only that it is NOT 5 or 6 here).
    let fx = Fixture::new();
    fx.init_git();
    fx.ccm()
        .args(["--gen-config", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);

    let assert = fx.ccm().args(["--config"]).arg(fx.config_dir()).assert();
    let code = assert.get_output().status.code().unwrap();
    assert!(code != 5 && code != 6, "unexpected exit code {code}");
}
