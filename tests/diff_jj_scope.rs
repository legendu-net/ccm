//! jj diff generation and `--include`/`--exclude` scope resolution through the real
//! binary (prd.md "Diff generation", "Diff scope resolution").

mod common;

use common::Fixture;
use predicates::prelude::*;
use std::fs;

#[test]
fn no_changes_is_exit_8() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.ccm()
        .args(["--config"])
        .arg(fx.config_dir())
        .assert()
        .code(8);
}

#[test]
fn changes_with_no_scope_flag_diffs_the_whole_working_copy() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.write("b.txt", "world\n");
    fx.ccm()
        .args(["--dry-run", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains("Generating diff using: jj")
                .and(predicate::str::contains("Enumerating").not()),
        );
}

#[test]
fn include_restricts_the_scope_to_the_matched_file() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.write("b.txt", "world\n");
    fx.ccm()
        .args(["--dry-run", "--include", "a.txt", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains("Generating diff using:")
                .and(predicate::str::contains("a.txt")),
        );
}

#[test]
fn include_a_typo_path_is_a_usage_error() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.ccm()
        .args(["--include", "typo.txt", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(2);
}

#[test]
fn exclude_a_typo_path_is_a_usage_error() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.ccm()
        .args(["--exclude", "typo.txt", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(2);
}

#[test]
fn excluding_every_changed_file_is_exit_8() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write("a.txt", "hello\n");
    fx.ccm()
        .args(["--exclude", "a.txt", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(8);
}

#[test]
fn include_a_directory_matches_nested_files() {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write("src/a.rs", "fn a() {}\n");
    fx.write("src/sub/b.rs", "fn b() {}\n");
    fx.write("other.rs", "fn other() {}\n");
    fx.ccm()
        .args(["--dry-run", "--include", "src", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0);
}

/// Renames a file with a shared directory/suffix (`dir/old/file.txt` ->
/// `dir/new/file.txt`) in a real jj working copy — the case that defeated the old
/// `jj diff --summary` brace-parsing approach (prd.md "Diff scope resolution"; see
/// `vcs/summary.rs`'s module doc). `jj new` checkpoints the original content so the
/// working-copy edit below diffs against a real prior commit, which is what makes jj's
/// own rename detection (and thus the `R` status line) kick in at all.
fn renamed_fixture() -> Fixture {
    let fx = Fixture::new();
    fx.init_jj();
    fx.write_valid_config();
    fx.write(
        "dir/old/file.txt",
        "hello world, this is a rename test with enough content to be detected as similar\n",
    );
    fx.jj_new("base");
    fs::create_dir_all(fx.repo_dir().join("dir/new")).expect("create dir/new");
    fs::rename(
        fx.repo_dir().join("dir/old/file.txt"),
        fx.repo_dir().join("dir/new/file.txt"),
    )
    .expect("rename file.txt");
    fs::remove_dir(fx.repo_dir().join("dir/old")).expect("remove now-empty dir/old");
    fx
}

#[test]
fn include_by_the_old_name_of_a_rename_selects_the_new_path() {
    let fx = renamed_fixture();
    fx.ccm()
        .args(["--dry-run", "--include", "dir/old/file.txt", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(0)
        .stderr(
            predicate::str::contains("Generating diff using:")
                .and(predicate::str::contains("dir/new/file.txt"))
                .and(predicate::str::contains("dir/old/file.txt").not()),
        );
}

#[test]
fn exclude_by_the_new_name_of_a_rename_is_exit_8() {
    let fx = renamed_fixture();
    fx.ccm()
        .args(["--exclude", "dir/new/file.txt", "--config"])
        .arg(fx.config_dir())
        .assert()
        .code(8);
}
