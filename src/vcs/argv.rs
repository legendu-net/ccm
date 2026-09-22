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

/// `jj --no-pager diff --color=never -T <template>` — the working-copy file enumeration
/// call, only used when `--include`/`--exclude` was given (or the interactive file
/// picker needs the candidate list). See [`ENUMERATE_TEMPLATE`]/[`ENUMERATE_FIELD_SEP`].
#[must_use]
pub fn jj_enumerate_args() -> Vec<String> {
    let mut args = strs(&["--no-pager", "diff", "--color=never", "-T"]);
    args.push(ENUMERATE_TEMPLATE.to_string());
    args
}

/// `jj --no-pager diff --color=never <files...>`. `files` is empty when neither
/// `--include` nor `--exclude` was given, in which case this diffs the whole working
/// copy exactly as `jj diff` would with no arguments.
#[must_use]
pub fn jj_diff_args(files: &[String]) -> Vec<String> {
    let mut args = strs(&["--no-pager", "diff", "--color=never"]);
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
/// empty, in which case it's a no-op trailing `--`.
#[must_use]
pub fn jj_commit_command_args(
    cmd: JjCommitCommand,
    message: &str,
    files: &[String],
) -> Vec<String> {
    match cmd {
        JjCommitCommand::Commit => with_files_args("commit", message, files),
        JjCommitCommand::Describe => vec![
            "describe".to_string(),
            "-m".to_string(),
            message.to_string(),
        ],
        JjCommitCommand::Split => with_files_args("split", message, files),
    }
}

fn with_files_args(subcommand: &str, message: &str, files: &[String]) -> Vec<String> {
    let mut args = vec![
        subcommand.to_string(),
        "-m".to_string(),
        message.to_string(),
        "--".to_string(),
    ];
    args.extend(files.iter().cloned());
    args
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
            jj_enumerate_args(),
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
            jj_diff_args(&[]),
            vec!["--no-pager", "diff", "--color=never"]
        );
    }

    #[test]
    fn jj_diff_args_with_files_appended() {
        assert_eq!(
            jj_diff_args(&["a.rs".to_string(), "b.rs".to_string()]),
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
        let rendered = render_command("jj", &jj_diff_args(&["my file.rs".to_string()]));
        let words = shell_words::split(&rendered).unwrap();
        assert_eq!(words.last().unwrap(), "my file.rs");
    }
}
