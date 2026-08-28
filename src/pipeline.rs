//! The check-order pipeline (prd.md "Check order"): a fixed sequence of stages where
//! the first failure wins. Each stage below corresponds to one numbered stage in the
//! spec, in the same order, so the source order of this function *is* the normative
//! check order — later stages are simply never reached once an earlier one returns
//! `Err` via `?`.
//!
//! This now covers the full spec: stage 1 (argument shape), the `--gen-config` and
//! `--list-tools` short-circuits, stage 2 (repository detection), stage 3
//! (repo-dependent argument validation), stage 4 (config load & validation), stage 5
//! (tool/API selection — an explicit `--tool <NAME>`, or the first enabled entry, or, in
//! default mode with an ambiguous choice, a tool-picker prompt), stage 6 (diff
//! generation — preceded, in default mode for a jj repository with no
//! `--include`/`--exclude` already given, by the interactive file picker,
//! `fileselect::resolve_interactively`), stage 7 (message generation), and stage 8
//! (`--dry-run`'s immediate blank check and raw stdout print, or the default mode's full
//! `$EDITOR` flow followed by the jj commit-command picker and the final commit
//! invocation).

use crate::cli::{self, Cli};
use crate::commit;
use crate::config::{self, gen_config, paths};
use crate::diff;
use crate::editor;
use crate::env::Environment;
use crate::error::{CcmError, EditorError, UsageError};
use crate::fileselect;
use crate::fzf;
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
/// picker. Uses the real `fzf` subprocess (`fzf::RealFzf`) as the tool picker's fuzzy
/// front-end — see [`run_with`] to substitute a different one (tests only; production
/// code should always call this).
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
    run_with(cli, env, stdin, out, stderr, &fzf::RealFzf)
}

/// Same as [`run`], but with the tool picker's fuzzy front-end injected — the seam
/// [`fzf::Fzf`] exists for (see its doc comment). Split out so `run`'s own signature
/// never has to change for this.
///
/// # Errors
/// Same as [`run`].
pub fn run_with(
    cli: &Cli,
    env: &dyn Environment,
    stdin: &mut dyn BufRead,
    out: &mut dyn std::io::Write,
    stderr: &mut dyn std::io::Write,
    fzf_picker: &dyn fzf::Fzf,
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
    // (see "Selection"), so an unmatched `--tool <NAME>` (or, under `--dry-run`, an
    // api.yaml with every entry disabled) fails fast before diff generation ever runs.
    // Three cases, in priority order:
    //   1. `--tool <NAME>` with a non-empty NAME overrides everything else outright,
    //      including selecting a disabled entry.
    //   2. `--tool ''` (an empty NAME) is a sentinel forcing the picker below
    //      regardless of `--dry-run` or how many entries are enabled — a deliberate,
    //      one-off request to browse every entry (including disabled ones) without
    //      touching api.yaml, so unlike case 3 it's allowed to need interactive stdin
    //      even under `--dry-run` (see "Interactive terminal requirement").
    //   3. Absent `--tool` entirely, `--dry-run` always takes the first enabled entry
    //      (there's no one to ask, and the Neovim wrapper needs this to run
    //      unattended); in default mode, the same first-enabled rule applies as long
    //      as there's no real choice to make — exactly one entry enabled; otherwise
    //      (zero enabled, or 2+ enabled) a human is presumably at the keyboard already
    //      (default mode already needs `$EDITOR` and, under jj, the commit-command
    //      picker), so `ccm` prompts for one of `loaded.entries` instead (never
    //      empty — an empty api.yaml is already `ConfigError::Empty` at stage 4).
    // The picker itself (case 2 or the ambiguous half of case 3) has two front-ends
    // (prd.md "Selection", "Preferences of Dependencies" item 8): `fzf`, shelled out to
    // as a subprocess when stdin is a real terminal and `CCM_FUZZY` isn't `0`, or the
    // numbered stdin prompt otherwise (including whenever `fzf` isn't on `$PATH` or
    // fails to run).
    let force_picker = matches!(cli.tool.as_deref(), Some(""));
    let selected = if let Some(name) = cli.tool.as_deref().filter(|name| !name.is_empty()) {
        config::validate::select_by_name(&loaded.entries, name)?
    } else if !force_picker
        && (cli.dry_run || config::validate::enabled_count(&loaded.entries) == 1)
    {
        config::validate::select_first_enabled(&loaded.entries)?
    } else {
        let lines = config::listing::lines(&loaded.entries);
        // A blank line at the prompt selects the same entry the `(default)` marker
        // above names — `None` when nothing is enabled, so a blank line just re-prompts
        // in that case, same as any other invalid input. `fzf` has no equivalent of a
        // blank line: Enter always confirms whichever candidate is highlighted.
        let default = config::listing::default_index(&loaded.entries);
        let fzf_enabled =
            env.stdin_is_terminal() && !fzf::disabled_by_env(env.var("CCM_FUZZY").as_deref());
        let index = if fzf_enabled {
            match fzf_picker.select(&cwd, &lines) {
                fzf::FzfOutcome::Selected(i) => i,
                fzf::FzfOutcome::Cancelled => {
                    return Err(picker::to_ccm_error(picker::PickerError::Cancelled));
                }
                fzf::FzfOutcome::Unavailable(reason) => {
                    let _ = progress::fzf_unavailable(stderr, &reason);
                    picker::prompt_index(stdin, stderr, &lines, default)
                        .map_err(picker::to_ccm_error)?
                }
            }
        } else {
            picker::prompt_index(stdin, stderr, &lines, default).map_err(picker::to_ccm_error)?
        };
        let _ = progress::section_break(stderr);
        &loaded.entries[index]
    };

    // Stage 6: diff generation, preceded by the interactive file picker (prd.md "Diff
    // scope resolution") — a fifth default-mode stdin prompt, jj-only, offered only
    // when neither `--include` nor `--exclude` already answered this question and
    // `--dry-run` isn't in play (same "no one to ask, don't block automation"
    // reasoning as every other prompt — see "Interactive terminal requirement").
    let mut selection = Selection::from_cli(&cli.include, &cli.exclude);
    if matches!(handling, RepoHandling::Jj { .. })
        && !cli.dry_run
        && matches!(selection, Selection::All)
        && env.stdin_is_terminal()
    {
        let fzf_enabled = !fzf::disabled_by_env(env.var("CCM_FUZZY").as_deref());
        // `raw` is always true here — the `env.stdin_is_terminal()` guard above is
        // exactly what `raw` means for every other picker in this pipeline (see the
        // stage-8 `let raw = env.stdin_is_terminal();` below).
        selection =
            fileselect::resolve_interactively(&cwd, stdin, stderr, fzf_picker, fzf_enabled, true)?;
    }
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
    // commits. Otherwise: the message review prompt (prd.md "Message review prompt") —
    // regenerate (re-run stage 7 against the same selected entry and the same diff,
    // then ask again), edit (the full $EDITOR flow, unchanged from before), or accept
    // (commit the message as-is, skipping $EDITOR entirely) — then straight to
    // `git commit` or the jj commit-command picker (never shown for a message that's
    // about to be discarded as blank, since it only runs once the message is already
    // confirmed non-blank).
    if cli.dry_run {
        if message.trim().is_empty() {
            return Err(EditorError::BlankMessage.into());
        }
        out.write_all(message.as_bytes())
            .map_err(|err| CcmError::Unexpected(err.to_string()))?;
        return Ok(());
    }

    let raw = env.stdin_is_terminal();
    let mut message = message;
    let final_message = loop {
        let blank = message.trim().is_empty();
        // A blank generation has nothing worth accepting (accepting it would just be
        // exit 15 with extra steps), so the prompt only offers regenerate/edit then —
        // see `picker::prompt_action`'s `allow_accept` parameter.
        let action =
            picker::prompt_action(stdin, stderr, !blank, raw).map_err(picker::to_ccm_error)?;
        let _ = progress::section_break(stderr);
        match action {
            picker::ReviewAction::Regenerate => {
                message = generation::generate(
                    selected,
                    &loaded.prompts,
                    &diff_result.diff,
                    &cwd,
                    env,
                    stderr,
                )?;
            }
            picker::ReviewAction::Edit => {
                let generated = (!blank).then_some(message.as_str());
                break editor::edit_message(generated, env)?;
            }
            picker::ReviewAction::Accept => {
                // Same cleanup/blank-check pair the edit path applies to whatever
                // $EDITOR saves (`src/editor/mod.rs`), so an accepted message commits
                // byte-identically either way, and the pathological "message is
                // nothing but #CCM: lines" case still hits exit 15.
                let cleaned = editor::message::cleanup(&message);
                if editor::message::is_blank(&cleaned) {
                    return Err(EditorError::BlankMessage.into());
                }
                break cleaned;
            }
        }
    };
    commit::commit(
        &handling,
        &final_message,
        &diff_result.files,
        &cwd,
        stdin,
        stderr,
        raw,
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

    fn write_all_disabled_config(config_dir: &std::path::Path) {
        std::fs::create_dir_all(config_dir).unwrap();
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
    }

    #[test]
    fn all_entries_disabled_under_dry_run_surfaces_as_exit_6() {
        // `--dry-run` never prompts (see "Interactive terminal requirement"), so with
        // nothing enabled it still fails fast at stage 5 exactly as before.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo/.git")).unwrap();
        let config_dir = tmp.path().join("config");
        write_all_disabled_config(&config_dir);
        let env = FakeEnvironment {
            cwd: tmp.path().join("repo"),
            ..FakeEnvironment::new()
        };
        let mut cli = base_cli();
        cli.dry_run = true;
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 6);
    }

    #[test]
    fn all_entries_disabled_in_default_mode_prompts_and_eof_cancels() {
        // Default mode still has a human to ask, so zero enabled entries triggers the
        // tool picker instead of failing outright; with stdin already at EOF, that
        // surfaces as the picker-cancelled exit code (14), not exit 6.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo/.git")).unwrap();
        let config_dir = tmp.path().join("config");
        write_all_disabled_config(&config_dir);
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
        assert_eq!(err.exit_code().as_u8(), 14);
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
    fn tool_flag_in_default_mode_with_two_enabled_entries_never_prompts() {
        // `--tool` must win outright regardless of `enabled_count`, in either mode: 2
        // enabled entries would otherwise make default mode's picker trigger, but with
        // `--tool` given, EOF'd stdin must not cancel anything — selection succeeds and
        // the run fails only later, at stage 6 (nothing staged, exit 8), proving the
        // picker was never reached.
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
        write_two_enabled_entry_config(&config_dir);
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
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Select a tool"));
    }

    fn write_two_enabled_entry_config(config_dir: &std::path::Path) {
        // Unlike `write_two_entry_config`, both entries are `enabled: true` — default
        // mode's tool picker only triggers when the choice is genuinely ambiguous (2+
        // entries enabled), so tests exercising that picker need this shape instead.
        std::fs::create_dir_all(config_dir).unwrap();
        std::fs::write(
            config_dir.join("prompts.yaml"),
            "default:\n  template: write it\n",
        )
        .unwrap();
        std::fs::write(
            config_dir.join("api.yaml"),
            "- name: a\n  type: openai_api\n  prompt: default\n  base_url: http://x\n  model: m\n  api_key:\n    env: K\n\
             - name: b\n  type: openai_api\n  prompt: default\n  base_url: http://y\n  model: m\n  api_key:\n    env: K\n",
        )
        .unwrap();
    }

    #[test]
    fn tool_picker_cancelled_on_eof_surfaces_as_exit_14() {
        // Default mode with 2+ enabled entries and no `--tool`: the ambiguous choice
        // must trigger the tool picker before diff generation (or `$EDITOR`) ever runs,
        // so an EOF'd stdin cancels the whole run at stage 5.
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
        write_two_enabled_entry_config(&config_dir);
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
        assert_eq!(err.exit_code().as_u8(), 14);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("Enter an index"));
    }

    #[test]
    fn single_enabled_entry_selects_silently_without_prompting() {
        // Exactly one entry enabled ("a"; "b" is disabled): default mode must not
        // prompt at all, so an EOF'd stdin doesn't cancel the run at stage 5 — it
        // reaches diff generation instead (nothing staged in a fresh repo, exit 8).
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
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Select a tool"));
    }

    #[test]
    fn empty_tool_flag_forces_the_picker_despite_a_single_enabled_entry() {
        // `--tool ''` is the force-picker sentinel: even with exactly one entry
        // enabled ("a"; "b" disabled) — which would otherwise silently select "a"
        // without prompting (see `single_enabled_entry_selects_silently_without_prompting`)
        // — it must show the picker, and let the user reach the disabled entry "b".
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
        cli.tool = Some(String::new());
        cli.config = Some(config_dir);
        // "1" picks "b", the disabled entry; nothing is staged, so selection succeeding
        // (rather than being skipped) surfaces as exit 8 at the next stage.
        let mut stdin = std::io::Cursor::new(b"1\n".to_vec());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("Enter an index"));
    }

    #[test]
    fn empty_tool_flag_forces_the_picker_even_under_dry_run() {
        // Unlike the automatic `--dry-run` path (no `--tool` at all), which never
        // prompts, `--tool ''` is a deliberate one-off ask and is allowed to need
        // interactive stdin even under `--dry-run` — so EOF'd stdin here cancels with
        // exit 14, not the usual dry-run silent-first-enabled behavior.
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
        cli.dry_run = true;
        cli.tool = Some(String::new());
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err = run(&cli, &env, &mut stdin, &mut out, &mut stderr).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("Enter an index"));
    }

    // ---- fzf front-end gating (fzf itself is never actually spawned here — these
    // exercise the seam `fzf::Fzf` exists for; see fzf.rs's own unit tests for the
    // exit-code-to-outcome mapping, and the plan's manual verification steps for real
    // interactive coverage, which can't be automated at all) ----

    struct FakeFzf(fzf::FzfOutcome);
    impl fzf::Fzf for FakeFzf {
        fn select(&self, _cwd: &std::path::Path, _lines: &[String]) -> fzf::FzfOutcome {
            self.0.clone()
        }
        // Every test using `FakeFzf` selects a tool over a git repo, which never
        // reaches the (jj-only) interactive file picker — see `fileselect_gating`
        // below for the double used there instead.
        fn select_files(&self, _cwd: &std::path::Path, _lines: &[String]) -> fzf::FzfMultiOutcome {
            panic!("select_files should not have been invoked in this test");
        }
    }

    /// Fails the test if either fzf front-end (tool picker or file picker) ever gets
    /// reached at all — used to guard the cases where neither must even be attempted
    /// (piped stdin, an unambiguous automatic selection, `--tool <NAME>` bypassing the
    /// tool picker outright, git handling bypassing the file picker outright, ...).
    struct PanickingFzf;
    impl fzf::Fzf for PanickingFzf {
        fn select(&self, _cwd: &std::path::Path, _lines: &[String]) -> fzf::FzfOutcome {
            panic!("fzf should not have been invoked in this test");
        }
        fn select_files(&self, _cwd: &std::path::Path, _lines: &[String]) -> fzf::FzfMultiOutcome {
            panic!("fzf should not have been invoked in this test");
        }
    }

    struct RecordingFzf {
        outcome: fzf::FzfOutcome,
        received: std::cell::RefCell<Option<Vec<String>>>,
    }
    impl fzf::Fzf for RecordingFzf {
        fn select(&self, _cwd: &std::path::Path, lines: &[String]) -> fzf::FzfOutcome {
            *self.received.borrow_mut() = Some(lines.to_vec());
            self.outcome.clone()
        }
        fn select_files(&self, _cwd: &std::path::Path, _lines: &[String]) -> fzf::FzfMultiOutcome {
            panic!("select_files should not have been invoked in this test");
        }
    }

    /// The file-picker analogue of `FakeFzf`: every test using this double drives the
    /// (jj-only) interactive file picker specifically, so its `select` (tool picker)
    /// side panics instead.
    struct FakeFilesFzf(fzf::FzfMultiOutcome);
    impl fzf::Fzf for FakeFilesFzf {
        fn select(&self, _cwd: &std::path::Path, _lines: &[String]) -> fzf::FzfOutcome {
            panic!("select should not have been invoked in this test");
        }
        fn select_files(&self, _cwd: &std::path::Path, _lines: &[String]) -> fzf::FzfMultiOutcome {
            self.0.clone()
        }
    }

    #[test]
    fn fzf_selection_picks_that_entry() {
        // Both entries enabled (ambiguous, so the picker triggers), terminal present:
        // fzf must be tried, and its answer must be what gets selected — not the
        // numbered prompt, which never runs (stdin is EOF'd here, so if the numbered
        // prompt ran instead it would cancel with exit 14, not reach diff generation).
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
        write_two_enabled_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let fzf_picker = FakeFzf(fzf::FzfOutcome::Selected(1));
        let err = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &fzf_picker).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Select a tool"));
    }

    #[test]
    fn fzf_receives_the_same_lines_list_tools_would_print() {
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
        write_two_enabled_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir.clone());
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let fzf_picker = RecordingFzf {
            outcome: fzf::FzfOutcome::Selected(0),
            received: std::cell::RefCell::new(None),
        };
        let _ = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &fzf_picker);
        let loaded = config::loader::load(&config_dir).unwrap();
        assert_eq!(
            fzf_picker.received.into_inner(),
            Some(config::listing::lines(&loaded.entries))
        );
    }

    #[test]
    fn fzf_cancelled_surfaces_as_exit_14() {
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
        write_two_enabled_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let fzf_picker = FakeFzf(fzf::FzfOutcome::Cancelled);
        let err = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &fzf_picker).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
    }

    #[test]
    fn fzf_unavailable_falls_back_to_the_numbered_prompt() {
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
        write_two_enabled_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(b"1\n".to_vec());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let fzf_picker = FakeFzf(fzf::FzfOutcome::Unavailable("fzf not found".to_string()));
        let err = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &fzf_picker).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("fzf unavailable"));
        assert!(printed.contains("Enter an index"));
    }

    #[test]
    fn fzf_unavailable_then_eof_still_exits_14() {
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
        write_two_enabled_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let fzf_picker = FakeFzf(fzf::FzfOutcome::Unavailable("fzf not found".to_string()));
        let err = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &fzf_picker).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
    }

    #[test]
    fn piped_stdin_never_reaches_fzf() {
        // stdin_is_terminal defaults to false — fzf must never even be attempted, so a
        // PanickingFzf must not panic; the picker falls straight to the numbered
        // prompt, and EOF'd stdin there cancels with exit 14 same as ever.
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
        write_two_enabled_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new()
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err =
            run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
    }

    #[test]
    fn ccm_fuzzy_zero_skips_fzf_for_the_numbered_prompt_even_on_a_terminal() {
        // A real terminal is present, so fzf would normally be tried — but `CCM_FUZZY=0`
        // forces the numbered prompt instead: a PanickingFzf must not fire, and the "1"
        // on stdin drives the numbered prompt straight through to diff generation.
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
        write_two_enabled_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new()
                .with_stdin_is_terminal(true)
                .with_var("CCM_FUZZY", "0")
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(b"1\n".to_vec());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err =
            run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("Enter an index"));
        assert!(!printed.contains("fzf unavailable"));
    }

    #[test]
    fn ccm_fuzzy_set_to_a_non_zero_value_still_uses_fzf() {
        // Only the exact value "0" is the escape hatch; anything else leaves fzf on.
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
        write_two_enabled_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new()
                .with_stdin_is_terminal(true)
                .with_var("CCM_FUZZY", "1")
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let fzf_picker = FakeFzf(fzf::FzfOutcome::Selected(1));
        let err = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &fzf_picker).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Select a tool"));
    }

    #[test]
    fn non_empty_tool_flag_bypasses_fzf() {
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
        write_two_enabled_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.tool = Some("b".to_string());
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err =
            run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
    }

    #[test]
    fn empty_tool_flag_forces_fzf_when_terminal() {
        // Exactly one entry enabled ("a"; "b" disabled) would otherwise select
        // silently without prompting at all — `--tool ''` must still force the picker,
        // and with a real terminal present, that means fzf specifically.
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
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.tool = Some(String::new());
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let fzf_picker = FakeFzf(fzf::FzfOutcome::Selected(1));
        let err = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &fzf_picker).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Select a tool"));
    }

    #[test]
    fn empty_tool_flag_forces_fzf_even_under_dry_run() {
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
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.dry_run = true;
        cli.tool = Some(String::new());
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let fzf_picker = FakeFzf(fzf::FzfOutcome::Cancelled);
        let err = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &fzf_picker).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
    }

    #[test]
    fn single_enabled_entry_never_reaches_fzf() {
        // Unambiguous automatic selection — fzf must not even be attempted, terminal
        // or not.
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
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err =
            run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
    }

    #[test]
    fn automatic_dry_run_never_reaches_fzf() {
        // No `--tool` at all: `--dry-run` always takes the first enabled entry
        // silently, terminal or not — fzf must not even be attempted.
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
        write_two_enabled_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.dry_run = true;
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err =
            run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
    }

    // ---- interactive file picker gating (prd.md "Diff scope resolution") — whether
    // the "restrict the diff to specific files?" prompt is even reached; its own
    // behavior (fzf outcomes, the "mark everything still pins an explicit list" rule,
    // the numbered fallback) is covered directly in fileselect.rs's own tests. All of
    // these leave the working
    // copy untouched (nothing staged/changed), and drive stdin with EOF whenever the
    // prompt is expected to be reached — deterministic and network-free: reaching the
    // prompt surfaces as exit 14 (cancelled on EOF, same as any other picker), never
    // reaching it surfaces as the pre-existing exit 8 (nothing to diff). ----

    fn jj_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let jj_config = tmp.path().join("jj.toml");
        std::fs::write(
            &jj_config,
            "[user]\nname = \"t\"\nemail = \"t@example.com\"\n",
        )
        .unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::process::Command::new("jj")
            .args(["git", "init", "--no-colocate"])
            .current_dir(&repo)
            .env("JJ_CONFIG", &jj_config)
            .status()
            .unwrap();
        (tmp, repo)
    }

    #[test]
    fn jj_default_mode_terminal_reaches_the_file_picker() {
        let (_tmp, repo) = jj_repo();
        // Two changed files: the picker only prompts once there's an actual choice
        // (see `jj_default_mode_terminal_with_one_changed_file_skips_the_picker`).
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let config_dir = repo.parent().unwrap().join("config");
        write_two_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new()); // EOF: cancels the prompt.
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err =
            run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("Restrict the diff to specific files?"));
    }

    #[test]
    fn jj_default_mode_terminal_with_one_changed_file_skips_the_picker() {
        // Restricting to a subset and diffing the whole working copy already mean
        // the same thing with only one file changed, so the prompt is skipped
        // outright. Since the one file really did change, diff generation succeeds
        // and the run proceeds into message generation (unlike every other gating
        // test here, which relies on "nothing staged" to stop early) — a 1s call
        // timeout on the (unreachable) `http://x` backend keeps this fast and
        // network-independent regardless of how that resolves; this test only
        // checks the prompt never appeared.
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        let config_dir = repo.parent().unwrap().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("prompts.yaml"),
            "default:\n  template: write it\n",
        )
        .unwrap();
        std::fs::write(
            config_dir.join("api.yaml"),
            "- name: a\n  type: openai_api\n  prompt: default\n  base_url: http://x\n  model: m\n  timeout: 1\n  api_key:\n    env: K\n",
        )
        .unwrap();
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let _ = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Restrict the diff to specific files?"));
    }

    #[test]
    fn dry_run_never_reaches_the_file_picker() {
        let (_tmp, repo) = jj_repo();
        let config_dir = repo.parent().unwrap().join("config");
        write_two_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.dry_run = true;
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err =
            run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Restrict the diff to specific files?"));
    }

    #[test]
    fn git_handling_never_reaches_the_file_picker() {
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
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err =
            run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Restrict the diff to specific files?"));
    }

    #[test]
    fn an_explicit_include_never_reaches_the_file_picker() {
        let (_tmp, repo) = jj_repo();
        let config_dir = repo.parent().unwrap().join("config");
        write_two_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.include = vec![PathBuf::from("a.rs")];
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        // Nothing changed at all, so the unmatched `--include` pattern is a usage
        // error (exit 2) rather than exit 8 — still well before message generation,
        // and still proof enough that the file picker (which would cancel with exit
        // 14 on this same EOF'd stdin) was never reached.
        let err =
            run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 2);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Restrict the diff to specific files?"));
    }

    #[test]
    fn piped_stdin_never_reaches_the_file_picker() {
        let (_tmp, repo) = jj_repo();
        let config_dir = repo.parent().unwrap().join("config");
        write_two_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new() // stdin_is_terminal defaults to false.
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(Vec::new());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let err =
            run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 8);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(!printed.contains("Restrict the diff to specific files?"));
    }

    #[test]
    fn jj_file_picker_answered_no_proceeds_to_diff_generation_unrestricted() {
        // Declining the prompt must behave exactly as if it had never been shown —
        // the diff generation call that follows must be the plain, unscoped
        // `jj diff` (no trailing file arguments), not one restricted to either file.
        // Two changed files (rather than nothing staged) so the prompt actually
        // appears; a 1s call timeout on the (unreachable) `http://x` backend keeps
        // this fast and network-independent regardless of how message generation,
        // which necessarily runs next, resolves.
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let config_dir = repo.parent().unwrap().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("prompts.yaml"),
            "default:\n  template: write it\n",
        )
        .unwrap();
        std::fs::write(
            config_dir.join("api.yaml"),
            "- name: a\n  type: openai_api\n  prompt: default\n  base_url: http://x\n  model: m\n  timeout: 1\n  api_key:\n    env: K\n",
        )
        .unwrap();
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(b"n\n".to_vec());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let _ = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &PanickingFzf);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("Restrict the diff to specific files?"));
        assert!(printed.contains("Generating diff using: jj --no-pager diff '--color=never'\n"));
    }

    #[test]
    fn jj_file_picker_answered_yes_reaches_fzf_and_cancelling_it_is_exit_14() {
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let config_dir = repo.parent().unwrap().join("config");
        write_two_entry_config(&config_dir);
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(b"y\n".to_vec());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        let fzf_picker = FakeFilesFzf(fzf::FzfMultiOutcome::Cancelled);
        let err = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &fzf_picker).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 14);
    }

    #[test]
    fn jj_file_picker_subset_selection_scopes_the_diff_generation_call() {
        // A successful subset selection must reach `diff::generate` with exactly the
        // marked file, not enumeration or a git-style unscoped diff — proven here by
        // the exact `Generating diff using:` progress line, which stage 6 only emits
        // once diff generation actually runs. A 1s call timeout on the (unreachable)
        // `http://x` backend keeps this test fast and network-independent regardless
        // of how message generation, which necessarily runs next, resolves.
        let (_tmp, repo) = jj_repo();
        std::fs::write(repo.join("a.rs"), "hello\n").unwrap();
        std::fs::write(repo.join("b.rs"), "world\n").unwrap();
        let config_dir = repo.parent().unwrap().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("prompts.yaml"),
            "default:\n  template: write it\n",
        )
        .unwrap();
        std::fs::write(
            config_dir.join("api.yaml"),
            "- name: a\n  type: openai_api\n  prompt: default\n  base_url: http://x\n  model: m\n  timeout: 1\n  api_key:\n    env: K\n",
        )
        .unwrap();
        let env = FakeEnvironment {
            cwd: repo,
            ..FakeEnvironment::new().with_stdin_is_terminal(true)
        };
        let mut cli = base_cli();
        cli.config = Some(config_dir);
        let mut stdin = std::io::Cursor::new(b"y\n".to_vec());
        let mut out = Vec::new();
        let mut stderr = Vec::new();
        // Candidate order is first-seen from `jj diff --summary`, alphabetical here:
        // index 0 is a.rs.
        let fzf_picker = FakeFilesFzf(fzf::FzfMultiOutcome::Selected(vec![0]));
        let _ = run_with(&cli, &env, &mut stdin, &mut out, &mut stderr, &fzf_picker);
        let printed = String::from_utf8(stderr).unwrap();
        assert!(printed.contains("Diff scope restricted to 1 file(s): a.rs"));
        assert!(printed.contains("Generating diff using: jj --no-pager diff '--color=never' a.rs"));
        assert!(!printed.contains("b.rs\n"));
    }
}
