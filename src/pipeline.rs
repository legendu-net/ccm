//! The check-order pipeline (prd.md "Check order"): a fixed sequence of stages where
//! the first failure wins. Each stage below corresponds to one numbered stage in the
//! spec, in the same order, so the source order of this function *is* the normative
//! check order — later stages are simply never reached once an earlier one returns
//! `Err` via `?`.
//!
//! This now covers the full spec: stage 1 (argument shape), the `--gen-config` and
//! `--list-tools` short-circuits, stage 2 (repository detection), stage 3
//! (repo-dependent argument validation), stage 4 (config load & validation), stage 5
//! (tool/API selection — the first enabled entry, or an explicit `--tool <NAME>`, or an
//! `--interactive` pick), stage 6 (diff generation), stage 7 (message generation), and
//! stage 8 (`--dry-run`'s immediate blank check and raw stdout print, or the default
//! mode's full `$EDITOR` flow followed by the jj commit-command picker and the final
//! commit invocation).

use crate::cli::{self, Cli};
use crate::commit;
use crate::config::{self, gen_config, paths};
use crate::diff;
use crate::editor;
use crate::env::Environment;
use crate::error::{CcmError, EditorError, UsageError};
use crate::generation;
use crate::picker;
use crate::progress;
use crate::repo::{self, RepoHandling};
use crate::vcs::scope::Selection;
use std::io::BufRead;

/// Runs the pipeline to completion. `out` receives whatever `ccm` would print to
/// stdout (`--gen-config`'s report, or the raw message under `--dry-run`); `stderr`
/// receives every progress line and the jj commit-command picker's prompts (prd.md
/// "Progress logging" — always stderr, regardless of `--dry-run`); `stdin` feeds the
/// picker.
///
/// # Errors
/// Any pipeline stage's failure, mapped to its documented exit code via
/// [`CcmError::exit_code`].
pub fn run(
    cli: &Cli,
    env: &dyn Environment,
    stdin: &mut dyn BufRead,
    out: &mut dyn std::io::Write,
    stderr: &mut dyn std::io::Write,
) -> Result<(), CcmError> {
    // Stage 1: argument-shape usage errors.
    cli::validate_shape(cli)?;

    // `--gen-config` short-circuits every later stage once it passes stage-1
    // validation, and does not require being run inside a git or jj repository.
    if cli.gen_config {
        let dir = paths::resolve_config_dir(cli.config.as_deref(), env)
            .map_err(|err| CcmError::Unexpected(format!("failed to resolve cwd: {err}")))?;
        let report = gen_config::gen_config(&dir)?;
        for line in report.lines() {
            writeln!(out, "{line}").map_err(|err| CcmError::Unexpected(err.to_string()))?;
        }
        return Ok(());
    }

    // `--list-tools` also short-circuits every later stage, for the same reason as
    // `--gen-config`: it only needs the config, not a repo, so it works from anywhere.
    // It still goes through stage-4 config load & validation (so a malformed api.yaml
    // is exit 5 here too), but never reaches repo detection or tool selection.
    if cli.list_tools {
        let dir = paths::resolve_config_dir(cli.config.as_deref(), env)
            .map_err(|err| CcmError::Unexpected(format!("failed to resolve cwd: {err}")))?;
        let loaded = config::loader::load(&dir)?;
        for line in config::listing::lines(&loaded.entries) {
            writeln!(out, "{line}").map_err(|err| CcmError::Unexpected(err.to_string()))?;
        }
        return Ok(());
    }

    // Stage 2: repository detection.
    let cwd = env
        .current_dir()
        .map_err(|err| CcmError::Unexpected(format!("failed to resolve cwd: {err}")))?;
    let roots = repo::locate_roots(&cwd)
        .map_err(|err| CcmError::Unexpected(format!("failed to resolve cwd: {err}")))?;
    let ctx = repo::classify(&roots)?;
    let handling = repo::apply_git_flag(ctx, cli.git)?;

    // Stage 3: repo-dependent argument validation.
    if matches!(handling, RepoHandling::Git { .. })
        && (!cli.include.is_empty() || !cli.exclude.is_empty())
    {
        return Err(UsageError::IncludeExcludeUnderGit.into());
    }

    // Stage 4: config load & validation.
    let config_dir = paths::resolve_config_dir_from(cli.config.as_deref(), &cwd, env);
    let loaded = config::loader::load(&config_dir)?;

    // Stage 5: tool/API selection — a trivial list scan with no probing of any kind
    // (see "Selection"), so an api.yaml with every entry disabled (or an unmatched
    // `--tool <NAME>`) fails fast before diff generation ever runs. `--tool` overrides
    // the first-enabled rule outright, including selecting a disabled entry; absent
    // that, `--interactive` prompts for one of `loaded.entries` (never empty — an empty
    // api.yaml is already `ConfigError::Empty` at stage 4).
    let selected = if let Some(name) = cli.tool.as_deref() {
        config::validate::select_by_name(&loaded.entries, name)?
    } else if cli.interactive {
        let lines = config::listing::lines(&loaded.entries);
        // A blank line at the prompt selects the same entry the `(default)` marker
        // above names — `None` when nothing is enabled, so a blank line just re-prompts
        // in that case, same as any other invalid input.
        let default = config::listing::default_index(&loaded.entries);
        let index =
            picker::prompt_index(stdin, stderr, &lines, default).map_err(picker::to_ccm_error)?;
        let _ = progress::blank_line(stderr);
        &loaded.entries[index]
    } else {
        config::validate::select_first_enabled(&loaded.entries)?
    };

    // Stage 6: diff generation.
    let selection = Selection::from_cli(&cli.include, &cli.exclude);
    let diff_result = diff::generate(&handling, &selection, &cwd, stderr)?;

    // Stage 7: message generation.
    let message = generation::generate(
        selected,
        &loaded.prompts,
        &diff_result.diff,
        &cwd,
        env,
        stderr,
    )?;

    // Stage 8. `message` has already been through stage 7's response cleanup
    // (`cleanup::clean_message` — see generation.rs), so there's no further stripping
    // to apply here. Under --dry-run: the empty-message check applies immediately
    // (there's no editor to give the user a chance to fix up a blank response), and the
    // cleaned response is printed to stdout exactly as `generation::generate` returned
    // it — no further stripping, trimming, or added trailing newline; --dry-run never
    // commits. Otherwise: the full $EDITOR flow to a cleaned, non-blank message, then
    // straight to `git commit` or the jj commit-command picker (never shown for a
    // message that's about to be discarded as blank, since it only runs once the
    // message is already confirmed non-blank).
    if cli.dry_run {
        if message.trim().is_empty() {
            return Err(EditorError::BlankMessage.into());
        }
        out.write_all(message.as_bytes())
            .map_err(|err| CcmError::Unexpected(err.to_string()))?;
        return Ok(());
    }

    let generated = (!message.trim().is_empty()).then_some(message.as_str());
    let cleaned_message = editor::edit_message(generated, env)?;
    commit::commit(
        &handling,
        &cleaned_message,
        &diff_result.files,
        &cwd,
        stdin,
        stderr,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::FakeEnvironment;
    use std::path::PathBuf;

    fn base_cli() -> Cli {
        Cli {
            include: vec![],
            exclude: vec![],
            git: false,
            dry_run: false,
            config: None,
            gen_config: false,
            list_tools: false,
            tool: None,
            interactive: false,
        }
    }

    #[test]
    fn not_a_repo_surfaces_as_exit_4() {
        let tmp = tempfile::tempdir().unwrap();
        let env = FakeEnvironment {
            cwd: tmp.path().to_path_buf(),
            ..FakeEnvironment::new()
        };
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&base_cli(), &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 4);
    }

    #[test]
    fn include_under_a_plain_git_repo_is_a_usage_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        let env = FakeEnvironment {
            cwd: tmp.path().to_path_buf(),
            ..FakeEnvironment::new()
        };
        let mut cli = base_cli();
        cli.include = vec![PathBuf::from("a")];
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 2);
    }

    #[test]
    fn missing_config_dir_surfaces_as_exit_5() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo/.git")).unwrap();
        let env = FakeEnvironment {
            cwd: tmp.path().join("repo"),
            ..FakeEnvironment::new()
        };
        let mut cli = base_cli();
        cli.config = Some(tmp.path().join("no-such-config-dir"));
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 5);
    }

    #[test]
    fn all_entries_disabled_surfaces_as_exit_6() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo/.git")).unwrap();
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("prompts.yaml"),
            "default:\n  template: write it\n",
        )
        .unwrap();
        std::fs::write(
            config_dir.join("api.yaml"),
            "- name: a\n  type: openai_api\n  enabled: false\n  prompt: default\n  base_url: http://x\n  model: m\n  api_key:\n    env: K\n",
        )
        .unwrap();
        let env = FakeEnvironment {
            cwd: tmp.path().join("repo"),
            ..FakeEnvironment::new()
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 6);
    }

    #[test]
    fn a_git_repo_with_nothing_staged_surfaces_as_exit_8() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("prompts.yaml"),
            "default:\n  template: write it\n",
        )
        .unwrap();
        std::fs::write(
            config_dir.join("api.yaml"),
            "- name: a\n  type: openai_api\n  prompt: default\n  base_url: http://x\n  model: m\n  api_key:\n    env: K\n",
        )
        .unwrap();
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new()
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
    }

    #[test]
    fn gen_config_short_circuits_before_repo_detection() {
        // cwd is not a repo at all; --gen-config must still succeed.
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("cfgdir");
        let env = FakeEnvironment {
            cwd: tmp.path().join("not-a-repo"),
            ..FakeEnvironment::new()
        };
        std::fs::create_dir_all(&env.cwd).unwrap();
        let mut cli = base_cli();
        cli.gen_config = true;
        cli.config = Some(target.clone());
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap();
        assert!(target.join("api.yaml").is_file());
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("created"));
    }

    fn write_two_entry_config(config_dir: &std::path::Path) {
        std::fs::create_dir_all(config_dir).unwrap();
        std::fs::write(
            config_dir.join("prompts.yaml"),
            "default:\n  template: write it\n",
        )
        .unwrap();
        std::fs::write(
            config_dir.join("api.yaml"),
            "- name: a\n  type: openai_api\n  prompt: default\n  base_url: http://x\n  model: m\n  api_key:\n    env: K\n\
             - name: b\n  type: openai_api\n  enabled: false\n  prompt: default\n  base_url: http://y\n  model: m\n  api_key:\n    env: K\n",
        )
        .unwrap();
    }

    #[test]
    fn list_tools_short_circuits_before_repo_detection() {
        // cwd is not a repo at all; --list-tools must still succeed.
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        write_two_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: tmp.path().join("not-a-repo"),
            ..FakeEnvironment::new()
        };
        std::fs::create_dir_all(&env.cwd).unwrap();
        let mut cli = base_cli();
        cli.list_tools = true;
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains('a'));
        assert!(printed.contains('b'));
        assert!(printed.contains("[enabled]"));
        assert!(printed.contains("[disabled]"));
        assert!(stderr.is_empty());
    }

    #[test]
    fn list_tools_with_malformed_config_surfaces_as_exit_5() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("prompts.yaml"), "default:\n  template: x\n").unwrap();
        std::fs::write(config_dir.join("api.yaml"), "not: [valid\n").unwrap();
        let env = FakeEnvironment {
            cwd: tmp.path().join("not-a-repo"),
            ..FakeEnvironment::new()
        };
        std::fs::create_dir_all(&env.cwd).unwrap();
        let mut cli = base_cli();
        cli.list_tools = true;
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 5);
    }

    #[test]
    fn unknown_tool_name_surfaces_as_exit_6() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let config_dir = tmp.path().join("config");
        write_two_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new()
        };
        let mut cli = base_cli();
        cli.tool = Some("no-such-tool".to_string());
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 6);
    }

    #[test]
    fn tool_flag_can_select_a_disabled_entry_and_reaches_diff_generation() {
        // "b" is disabled but still selectable via --tool; with nothing staged in a
        // fresh git repo, selection must succeed and fail only later, at stage 6
        // (nothing to diff, exit 8) — proving --tool bypassed exit 6 outright.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        let config_dir = tmp.path().join("config");
        write_two_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new()
        };
        let mut cli = base_cli();
        cli.tool = Some("b".to_string());
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
    }

    #[test]
    fn interactive_picker_cancelled_on_eof_surfaces_as_exit_14() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        let config_dir = tmp.path().join("config");
        write_two_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new()
        };
        let mut cli = base_cli();
        cli.interactive = true;
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("Select a tool"));
    }
}
