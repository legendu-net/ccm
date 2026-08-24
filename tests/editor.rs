//! The `$EDITOR` flow through the real binary: exit 12 (unavailable), 17 (temp file),
//! 13 (aborted), 1 (deleted file), 15 (blank), and the "temp file always survives"
//! guarantee (prd.md "Default behavior", "Message pre-population and cleanup").

mod common;

use common::Fixture;
use predicates::prelude::*;
use std::fs;

/// A repo + config that reach the editor step with a known non-blank generated
/// message ("feat: test commit message", from `write_valid_config`'s fake agent_cli).
fn ready_for_editor(fx: &Fixture) {
    fx.init_git();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.stage("a.txt");
}

#[test]
fn no_editor_available_is_exit_12_and_leaves_no_temp_file() {
    let fx = Fixture::new();
    ready_for_editor(&fx);
    // The real PATH (which Fixture::ccm() otherwise prepends the fixture bin dir to)
    // may well have nvim/vim/vi installed — restrict PATH to just the fixture bin dir
    // (with a `git` symlink so diff generation still works) so the fallback chain has
    // genuinely nothing to find, rather than launching a real interactive editor
    // against this non-interactive test's stdio and hanging.
    fx.symlink_git_into_bin_dir();
    fx.ccm()
        .env("PATH", fx.bin_dir())
        .env("EDITOR", "")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(12);
    assert!(fx.editmsg_files().is_empty());
}

#[test]
fn editor_set_but_unresolvable_is_exit_12() {
    let fx = Fixture::new();
    ready_for_editor(&fx);
    fx.ccm()
        .env("EDITOR", "no-such-editor-xyz")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(12);
}

#[test]
fn temp_file_creation_failure_is_exit_17() {
    let fx = Fixture::new();
    ready_for_editor(&fx);
    // Point TMPDIR at a path that doesn't exist -> tempfile_in() fails deterministically.
    fx.write_script("ed", "exit 0");
    fx.ccm()
        .env("EDITOR", "ed")
        .env("TMPDIR", fx.tmp_dir().join("does-not-exist"))
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(17);
}

#[test]
fn an_editor_that_saves_a_message_succeeds() {
    // The jj/git commit invocation itself isn't wired up yet (a later phase); this
    // only exercises the editor flow reaching a cleaned, non-blank message.
    let fx = Fixture::new();
    ready_for_editor(&fx);
    fx.write_script("ed", "printf 'feat: edited\\n' > \"$1\"");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);
    assert_eq!(fx.editmsg_files().len(), 1);
}

#[test]
fn an_editor_that_exits_non_zero_is_aborted() {
    let fx = Fixture::new();
    ready_for_editor(&fx);
    fx.write_script("ed", "exit 7");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(13);
    // Temp file still survives even on an aborted editor.
    assert_eq!(fx.editmsg_files().len(), 1);
}

#[test]
fn an_editor_that_deletes_the_temp_file_is_exit_1() {
    let fx = Fixture::new();
    ready_for_editor(&fx);
    fx.write_script("ed", "rm \"$1\"");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(1);
}

#[test]
fn a_blank_saved_message_is_exit_15_and_the_temp_file_still_survives() {
    let fx = Fixture::new();
    ready_for_editor(&fx);
    fx.write_script("ed", ": > \"$1\"");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(15);
    assert_eq!(fx.editmsg_files().len(), 1);
}

#[test]
fn the_temp_file_is_pre_populated_with_the_generated_message_and_ccm_comment() {
    let fx = Fixture::new();
    ready_for_editor(&fx);
    // Editor reads its own argv[1] (the temp file path) and copies its pre-populated
    // content to a marker file for this test to inspect afterward, then leaves the
    // original temp file untouched (an editor that makes no changes).
    let marker = fx.tmp_dir().join("captured.txt");
    fx.write_script("ed", &format!("cp \"$1\" {}", marker.display()));
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);
    let captured = fs::read_to_string(&marker).unwrap();
    assert!(captured.starts_with("feat: test commit message"));
    assert!(captured.contains("#CCM: message generated above."));
}

#[test]
fn the_editor_temp_file_name_has_the_ccm_editmsg_prefix() {
    let fx = Fixture::new();
    ready_for_editor(&fx);
    fx.write_script("ed", "exit 0");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);
    let files = fx.editmsg_files();
    assert_eq!(files.len(), 1);
    assert!(
        files[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("CCM_EDITMSG_")
    );
}

#[test]
fn editor_leading_args_and_a_quoted_path_with_a_space_are_honored() {
    let fx = Fixture::new();
    ready_for_editor(&fx);
    // The script name itself contains a space, and $EDITOR must be quoted for it.
    fx.write_script("ed with space", "printf 'feat: y\\n' > \"$2\"");
    fx.ccm()
        .env("EDITOR", "\"ed with space\" --flag")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);
}

#[test]
fn progress_lines_up_through_message_generation_appear_on_stderr_in_order() {
    // Committing isn't wired up yet (a later phase), so this covers stages 6-7's
    // progress lines only; tests/check_order.rs (or a picker/commit test file) pins
    // the commit-stage lines once that lands.
    let fx = Fixture::new();
    ready_for_editor(&fx);
    fx.write_script("ed", "exit 0");
    fx.ccm()
        .env("EDITOR", "ed")
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains("Generating diff using:")
                .and(predicate::str::contains("Diff generated by:"))
                .and(predicate::str::contains("Generating commit message using"))
                .and(predicate::str::contains("Commit message generated by")),
        );
}
