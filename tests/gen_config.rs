//! `--gen-config` — exit 0 (success, incl. partial/no-op runs) and exit 3 (filesystem
//! failure). Also proves it does not require being run inside a git or jj repository:
//! the fixture repo dir is never `git init`/`jj git init`'d in this file.

mod common;

use common::Fixture;
use predicates::prelude::*;
use std::fs;

#[test]
fn creates_both_files_in_a_fresh_directory_and_does_not_require_a_repo() {
    let fx = Fixture::new();
    let dir = fx.root_config_target();

    fx.ccm()
        .args(["--gen-config", "--config"])
        .arg(&dir)
        .assert()
        .code(0)
        .stdout(
            predicate::str::contains("prompts.yaml: created")
                .and(predicate::str::contains("api.yaml: created")),
        )
        .stderr(predicate::str::is_empty());

    assert!(dir.join("prompts.yaml").is_file());
    assert!(dir.join("api.yaml").is_file());
    let api = fs::read_to_string(dir.join("api.yaml")).unwrap();
    assert!(api.contains("name: omniroute"));
}

#[test]
fn never_overwrites_files_that_already_exist() {
    let fx = Fixture::new();
    let dir = fx.root_config_target();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("prompts.yaml"), "MY CUSTOM PROMPTS").unwrap();
    fs::write(dir.join("api.yaml"), "MY CUSTOM API").unwrap();

    fx.ccm()
        .args(["--gen-config", "--config"])
        .arg(&dir)
        .assert()
        .code(0)
        .stdout(
            predicate::str::contains("prompts.yaml: already existed, left untouched").and(
                predicate::str::contains("api.yaml: already existed, left untouched"),
            ),
        );

    assert_eq!(
        fs::read_to_string(dir.join("prompts.yaml")).unwrap(),
        "MY CUSTOM PROMPTS"
    );
    assert_eq!(
        fs::read_to_string(dir.join("api.yaml")).unwrap(),
        "MY CUSTOM API"
    );
}

#[test]
fn reports_a_partial_run_explicitly() {
    let fx = Fixture::new();
    let dir = fx.root_config_target();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("api.yaml"), "MY CUSTOM API").unwrap();

    fx.ccm()
        .args(["--gen-config", "--config"])
        .arg(&dir)
        .assert()
        .code(0)
        .stdout(
            predicate::str::contains("prompts.yaml: created")
                .and(predicate::str::contains("api.yaml: already existed")),
        );
}

#[test]
fn fails_with_exit_3_when_target_is_a_regular_file() {
    let fx = Fixture::new();
    let path = fx.root_config_target();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, "not a directory").unwrap();

    fx.ccm()
        .args(["--gen-config", "--config"])
        .arg(&path)
        .assert()
        .code(3)
        .stdout(predicate::str::is_empty());
}

#[test]
fn uses_default_config_dir_when_no_config_flag_given() {
    let fx = Fixture::new();
    fx.ccm()
        .arg("--gen-config")
        .assert()
        .code(0)
        .stdout(predicate::str::contains("created"));
    assert!(fx.xdg_config_dir().join("api.yaml").is_file());
}
