//! Pure argv builders for every git/jj invocation `ccm` makes (prd.md "Diff
//! generation", "git commit", "jj commit commands"). These are fixed argument
//! templates — never a shell command line — so `{{message}}`/`{{files}}` substitution
//! is always a literal `Command::arg()`/`args()` entry, safe for an unescaped
//! multi-line message or a path containing spaces or shell metacharacters.

/// `git --no-pager diff --no-color --staged` (no file arguments — git handling never
/// accepts `--include`/`--exclude`).
#[must_use]
pub fn git_diff_args() -> Vec<String> {
    strs(&["--no-pager", "diff", "--no-color", "--staged"])
}

/// `git commit -m {{message}}`.
#[must_use]
pub fn git_commit_args(message: &str) -> Vec<String> {
    vec!["commit".to_string(), "-m".to_string(), message.to_string()]
}

/// `\x1f` (ASCII "unit separator") — the field delimiter between the status character
/// and the two paths in [`jj_enumerate_args`]'s template output. Chosen over reusing
/// jj's own human-oriented `--summary` brace notation (`{<old> => <new>}`) for
/// renames/copies, which turned out to be genuinely ambiguous to parse back for some
/// real filenames — jj does not escape a literal `{`/`}` a filename itself contains, so
/// depth/boundary heuristics over that notation could still be fooled by a sufficiently
/// adversarial path (prd.md "Diff scope resolution" has the history). A control
/// character reserved by the ASCII standard for exactly this purpose, and one no
/// realistic path will ever contain, sidesteps that whole class of problem instead of
/// trying to out-clever it.
pub const ENUMERATE_FIELD_SEP: char = '\u{1f}';

/// jj template-language source for [`jj_enumerate_args`]'s `-T` flag: one
/// `<status-char><SEP><source-path><SEP><target-path>` record per line (`\n`-terminated,
/// `SEP` = [`ENUMERATE_FIELD_SEP`]). `source`/`target` are jj's own `TreeDiffEntry`
/// "left"/"right" accessors — equal to each other for `M`/`A`/`D`, the old/new path for
/// `R`/`C` — and `.display()` reports each path relative to the current working
/// directory, the same basis `jj diff --summary` used.
const ENUMERATE_TEMPLATE: &str = "status_char ++ \"\\x1f\" ++ source.path().display() \
     ++ \"\\x1f\" ++ target.path().display() ++ \"\\n\"";

/// `--revision`'s value once `diff::check_revision` has accepted it: the revset as the
/// user typed it, plus whether it resolves to the working-copy revision (`@`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revision<'a> {
    pub revset: &'a str,
    pub is_working_copy: bool,
}

/// `jj --no-pager --ignore-working-copy log --no-graph --color=never --limit 2 -r
/// <revision> -T <template>` — the `--revision` check (prd.md "Diff generation"):
/// prints one commit id per line for the revisions `revision` resolves to. `--limit 2`
/// because the only question asked of the output is "exactly one, or not" — there's no
/// point printing an arbitrarily large revset (e.g. `all()`) in full just to reject it.
/// `--ignore-working-copy` because nothing the check reports depends on a fresh
/// working-copy snapshot (see [`jj_read_prefix`]).
#[must_use]
pub fn jj_revision_check_args(revision: &str) -> Vec<String> {
    let mut args = strs(&[
        "--no-pager",
        "--ignore-working-copy",
        "log",
        "--no-graph",
        "--color=never",
        "--limit",
        "2",
        "-r",
    ]);
    args.push(revision.to_string());
    args.push("-T".to_string());
    args.push(REVISION_CHECK_TEMPLATE.to_string());
    args
}

/// jj template-language source for [`jj_revision_check_args`]'s `-T` flag: one line per
/// revision, its commit id, whether it's immutable and whether it's the working-copy
/// revision (`true`/`false` each), space-separated.
const REVISION_CHECK_TEMPLATE: &str =
    "commit_id ++ \" \" ++ immutable ++ \" \" ++ current_working_copy ++ \"\\n\"";

/// `--no-pager`, plus `--ignore-working-copy` when `revision` is a revision other than
/// the working copy. Every jj command snapshots the working copy first by default, which
/// can record a new operation — concurrent with an outer `jj split`/`jj describe` whose
/// editor session is running `ccm` (the Neovim wrapper). A non-working-copy revision's
/// diff doesn't depend on that snapshot, so the read-only `jj diff` calls skip it there.
fn jj_read_prefix(revision: Option<Revision<'_>>) -> Vec<String> {
    let mut args = strs(&["--no-pager"]);
    if revision.is_some_and(|revision| !revision.is_working_copy) {
        args.push("--ignore-working-copy".to_string());
    }
    args
}

/// `jj --no-pager [--ignore-working-copy] diff --color=never [-r <revision>] -T
/// <template>` — the changed-file
/// enumeration call, only used when `--include`/`--exclude` was given (or the
/// interactive file picker needs the candidate list). `revision` is `--revision`'s
/// value, if given; `None` enumerates the working copy. See
/// [`ENUMERATE_TEMPLATE`]/[`ENUMERATE_FIELD_SEP`] and [`jj_read_prefix`].
#[must_use]
pub fn jj_enumerate_args(revision: Option<Revision<'_>>) -> Vec<String> {
    let mut args = jj_read_prefix(revision);
    args.extend(strs(&["diff", "--color=never"]));
    push_revision(&mut args, revision.map(|revision| revision.revset));
    args.push("-T".to_string());
    args.push(ENUMERATE_TEMPLATE.to_string());
    args
}

/// `jj --no-pager [--ignore-working-copy] diff --color=never [-r <revision>]
/// <files...>`. `files` is empty when neither `--include` nor `--exclude` was given, in
/// which case this diffs the whole revision (the working copy when `revision` is `None`)
/// exactly as `jj diff [-r <revision>]` would with no file arguments. See
/// [`jj_read_prefix`].
#[must_use]
pub fn jj_diff_args(revision: Option<Revision<'_>>, files: &[String]) -> Vec<String> {
    let mut args = jj_read_prefix(revision);
    args.extend(strs(&["diff", "--color=never"]));
    push_revision(&mut args, revision.map(|revision| revision.revset));
    args.extend(files.iter().cloned());
    args
}

/// Which jj command the commit-command picker chose (prd.md "jj commit commands").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JjCommitCommand {
    Commit,
    Describe,
    Split,
}

/// `jj commit -m {{message}} -- {{files}}`, `jj describe -m {{message}}` (no
/// `{{files}}` slot), or `jj split -m {{message}} -- {{files}}`. A literal `--`
/// precedes `{{files}}` for `commit`/`split` so a resolved path beginning with `-` is
/// still parsed as a positional fileset argument — harmless even when `files` is
/// empty, in which case it's a no-op trailing `--`. See [`jj_revision_command_args`]
/// under `--revision`.
#[must_use]
pub fn jj_commit_command_args(
    cmd: JjCommitCommand,
    message: &str,
    files: &[String],
) -> Vec<String> {
    match cmd {
        JjCommitCommand::Commit => with_files_args("commit", message, None, files),
        JjCommitCommand::Describe => describe_args(message, None),
        JjCommitCommand::Split => with_files_args("split", message, None, files),
    }
}

/// Under `--revision`, the one applicable command (prd.md "jj commit commands"): `jj
/// split -m {{message}} -r {{revision}} -- {{files}}` when `files` is non-empty, `jj
/// describe -m {{message}} -r {{revision}}` otherwise. `jj commit` only ever acts on
/// the working copy, so it's never an option here.
#[must_use]
pub fn jj_revision_command_args(
    revision: Revision<'_>,
    message: &str,
    files: &[String],
) -> Vec<String> {
    if files.is_empty() {
        describe_args(message, Some(revision.revset))
    } else {
        with_files_args("split", message, Some(revision.revset), files)
    }
}

fn describe_args(message: &str, revision: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "describe".to_string(),
        "-m".to_string(),
        message.to_string(),
    ];
    push_revision(&mut args, revision);
    args
}

fn with_files_args(
    subcommand: &str,
    message: &str,
    revision: Option<&str>,
    files: &[String],
) -> Vec<String> {
    let mut args = vec![
        subcommand.to_string(),
        "-m".to_string(),
        message.to_string(),
    ];
    push_revision(&mut args, revision);
    args.push("--".to_string());
    args.extend(files.iter().cloned());
    args
}

/// Appends `-r <revision>` when `--revision` was given; nothing otherwise, so every
/// argv stays byte-identical to its pre-`--revision` form in the common case.
fn push_revision(args: &mut Vec<String>, revision: Option<&str>) {
    if let Some(revision) = revision {
        args.push("-r".to_string());
        args.push(revision.to_string());
    }
}

/// Renders `program`/`args` as a single shell-quoted, copy-pasteable string for
/// progress-line logging (prd.md "Progress logging": `Generating diff using: <command>`
/// etc.) — this is purely for display; `ccm` never builds or runs this as an actual
/// shell command line.
#[must_use]
pub fn render_command(program: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(program.to_string());
    parts.extend(args.iter().cloned());
    shell_words::join(parts)
}

fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::PARENT;

    #[test]
    fn git_diff_args_match_the_spec_exactly() {
        assert_eq!(
            git_diff_args(),
            vec!["--no-pager", "diff", "--no-color", "--staged"]
        );
    }

    #[test]
    fn git_commit_args_carry_the_message_as_one_arg() {
        assert_eq!(
            git_commit_args("feat: add x\n\nbody"),
            vec!["commit", "-m", "feat: add x\n\nbody"]
        );
    }

    #[test]
    fn jj_enumerate_args_match_the_spec_exactly() {
        assert_eq!(
            jj_enumerate_args(None),
            vec![
                "--no-pager",
                "diff",
                "--color=never",
                "-T",
                ENUMERATE_TEMPLATE,
            ]
        );
    }

    #[test]
    fn enumerate_template_uses_the_field_sep_constant_consistently() {
        // Pins that the template's `\x1f` escapes and `ENUMERATE_FIELD_SEP` agree —
        // `summary.rs` parses by splitting on the constant, not by re-deriving it.
        assert_eq!(ENUMERATE_FIELD_SEP, '\u{1f}');
        assert_eq!(ENUMERATE_TEMPLATE.matches("\\x1f").count(), 2);
    }

    #[test]
    fn jj_diff_args_with_no_files() {
        assert_eq!(
            jj_diff_args(None, &[]),
            vec!["--no-pager", "diff", "--color=never"]
        );
    }

    #[test]
    fn jj_diff_args_with_files_appended() {
        assert_eq!(
            jj_diff_args(None, &["a.rs".to_string(), "b.rs".to_string()]),
            vec!["--no-pager", "diff", "--color=never", "a.rs", "b.rs"]
        );
    }

    #[test]
    fn jj_commit_has_a_trailing_dashdash_even_with_no_files() {
        assert_eq!(
            jj_commit_command_args(JjCommitCommand::Commit, "msg", &[]),
            vec!["commit", "-m", "msg", "--"]
        );
    }

    #[test]
    fn jj_commit_includes_files_after_dashdash() {
        assert_eq!(
            jj_commit_command_args(JjCommitCommand::Commit, "msg", &["a.rs".to_string()]),
            vec!["commit", "-m", "msg", "--", "a.rs"]
        );
    }

    #[test]
    fn jj_describe_has_no_files_slot() {
        assert_eq!(
            jj_commit_command_args(JjCommitCommand::Describe, "msg", &["a.rs".to_string()]),
            vec!["describe", "-m", "msg"]
        );
    }

    #[test]
    fn jj_split_matches_commit_shape() {
        assert_eq!(
            jj_commit_command_args(JjCommitCommand::Split, "msg", &[]),
            vec!["split", "-m", "msg", "--"]
        );
    }

    #[test]
    fn jj_revision_check_args_match_the_spec_exactly() {
        assert_eq!(
            jj_revision_check_args("@-"),
            vec![
                "--no-pager",
                "--ignore-working-copy",
                "log",
                "--no-graph",
                "--color=never",
                "--limit",
                "2",
                "-r",
                "@-",
                "-T",
                "commit_id ++ \" \" ++ immutable ++ \" \" ++ current_working_copy ++ \"\\n\"",
            ]
        );
    }

    #[test]
    fn jj_enumerate_args_with_a_revision() {
        assert_eq!(
            jj_enumerate_args(Some(PARENT)),
            vec![
                "--no-pager",
                "--ignore-working-copy",
                "diff",
                "--color=never",
                "-r",
                "@-",
                "-T",
                ENUMERATE_TEMPLATE,
            ]
        );
    }

    #[test]
    fn jj_diff_args_with_a_revision_precede_the_files() {
        assert_eq!(
            jj_diff_args(Some(PARENT), &["a.rs".to_string()]),
            vec![
                "--no-pager",
                "--ignore-working-copy",
                "diff",
                "--color=never",
                "-r",
                "@-",
                "a.rs",
            ]
        );
    }

    #[test]
    fn jj_diff_args_with_the_working_copy_revision_still_snapshot() {
        let working_copy = Revision {
            revset: "@",
            is_working_copy: true,
        };
        assert_eq!(
            jj_diff_args(Some(working_copy), &[]),
            vec!["--no-pager", "diff", "--color=never", "-r", "@"]
        );
        assert_eq!(
            jj_enumerate_args(Some(working_copy)),
            vec![
                "--no-pager",
                "diff",
                "--color=never",
                "-r",
                "@",
                "-T",
                ENUMERATE_TEMPLATE,
            ]
        );
    }

    #[test]
    fn jj_revision_unscoped_describes() {
        assert_eq!(
            jj_revision_command_args(PARENT, "msg", &[]),
            vec!["describe", "-m", "msg", "-r", "@-"]
        );
    }

    #[test]
    fn jj_revision_scoped_splits_with_the_revision_before_dashdash() {
        assert_eq!(
            jj_revision_command_args(PARENT, "msg", &["a.rs".to_string()]),
            vec!["split", "-m", "msg", "-r", "@-", "--", "a.rs"]
        );
    }

    #[test]
    fn render_command_quotes_a_multiline_message() {
        let args = git_commit_args("feat: x\n\nbody line");
        let rendered = render_command("git", &args);
        assert!(rendered.starts_with("git commit -m"));
        // shell_words quoting round-trips back to the exact original message.
        let words = shell_words::split(&rendered).unwrap();
        assert_eq!(words, vec!["git", "commit", "-m", "feat: x\n\nbody line"]);
    }

    #[test]
    fn render_command_handles_paths_with_spaces() {
        let rendered = render_command("jj", &jj_diff_args(None, &["my file.rs".to_string()]));
        let words = shell_words::split(&rendered).unwrap();
        assert_eq!(words.last().unwrap(), "my file.rs");
    }
}
