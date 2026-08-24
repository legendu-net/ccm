//! jj diff generation and `--include`/`--exclude` scope resolution through the real
//! binary (prd.md "Diff generation", "Diff scope resolution").

mod common;

use common::Fixture;
use predicates::prelude::*;

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
fn include_restricts_the_scope_and_enumerates_first() {
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
            predicate::str::contains("Enumerating working-copy files using:").and(
                predicate::str::contains("Working-copy files enumerated by:"),
            ),
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
