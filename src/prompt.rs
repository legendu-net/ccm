//! Prompt rendering: the resolved `prompts.yaml` entry, the user message sent to both
//! backend types, and `{{model}}`/`{{system}}` substitution for `agent_cli` `args`
//! (prd.md, `prompts.yaml` and `type: agent_cli` fields under Configuration). Pure.

/// A resolved `prompts.yaml` entry, borrowed for the duration of one generation call.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedPrompt<'a> {
    pub system: Option<&'a str>,
    pub template: &'a str,
}

/// The user-role message sent to both backend types: `template` verbatim (no trimming
/// of its own trailing whitespace), a `\n\n` separator, then the diff verbatim — no
/// `Diff:` label, since the diff itself already makes clear where it starts. For
/// `openai_api` this is the `user` role message; for `agent_cli` it's written to the
/// child process's stdin, which is then closed.
#[must_use]
pub fn user_message(template: &str, diff: &str) -> String {
    format!("{template}\n\n{diff}")
}

/// Substitutes `{{model}}` and `{{system}}` (`system`, or an empty string if `None`)
/// into each `agent_cli` `args` entry. An arg with no placeholder is returned unchanged;
/// an arg can contain more than one placeholder.
#[must_use]
pub fn substitute_args(args: &[String], system: Option<&str>, model: &str) -> Vec<String> {
    args.iter()
        .map(|arg| substitute_one(arg, system, model))
        .collect()
}

/// A single left-to-right pass over `arg`, rather than two sequential `str::replace`
/// calls: sequential whole-string replacement would re-scan text a prior substitution
/// just inserted, so a `system` value that itself happens to contain the literal
/// substring `{{model}}` would get corrupted by the later replacement pass. Scanning
/// once and advancing past each match (never revisiting already-emitted output) keeps
/// every substituted value's own text untouched.
fn substitute_one(arg: &str, system: Option<&str>, model: &str) -> String {
    const PLACEHOLDERS: [&str; 2] = ["{{model}}", "{{system}}"];
    let mut result = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some((offset, token)) = PLACEHOLDERS
        .iter()
        .filter_map(|token| rest.find(token).map(|offset| (offset, *token)))
        .min_by_key(|(offset, _)| *offset)
    {
        let value = match token {
            "{{model}}" => model,
            "{{system}}" => system.unwrap_or(""),
            _ => unreachable!("token is always one of PLACEHOLDERS"),
        };
        result.push_str(&rest[..offset]);
        result.push_str(value);
        rest = &rest[offset + token.len()..];
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_joins_template_and_diff_with_a_blank_line() {
        assert_eq!(
            user_message("Write a commit message.", "diff --git a/x b/x"),
            "Write a commit message.\n\ndiff --git a/x b/x"
        );
    }

    #[test]
    fn user_message_does_not_trim_template_trailing_whitespace() {
        assert_eq!(user_message("Write it.  ", "DIFF"), "Write it.  \n\nDIFF");
    }

    #[test]
    fn a_system_value_containing_literal_placeholder_syntax_survives_intact() {
        // Regression test: sequential whole-string .replace() calls would let a later
        // pass re-scan and corrupt text a prior pass just inserted. A system value that
        // itself mentions "{{model}}" (e.g. a meta-prompt about ccm's own syntax) must
        // come through byte-for-byte once substituted for {{system}}.
        let system = Some("Explain what {{model}} means in this context.");
        let args = vec!["--system".to_string(), "{{system}}".to_string()];
        let result = substitute_args(&args, system, "gpt-5.4");
        assert_eq!(
            result,
            vec!["--system", "Explain what {{model}} means in this context."]
        );
    }

    #[test]
    fn substitute_args_replaces_model() {
        let args = vec!["-m".to_string(), "{{model}}".to_string()];
        let result = substitute_args(&args, None, "gemini-2.5-pro");
        assert_eq!(result, vec!["-m", "gemini-2.5-pro"]);
    }

    #[test]
    fn substitute_args_fills_system_when_present() {
        let args = vec!["--system".to_string(), "{{system}}".to_string()];
        let result = substitute_args(&args, Some("You are helpful."), "m");
        assert_eq!(result, vec!["--system", "You are helpful."]);
    }

    #[test]
    fn substitute_args_uses_empty_string_when_system_placeholder_present_but_unset() {
        let args = vec!["--system".to_string(), "{{system}}".to_string()];
        let result = substitute_args(&args, None, "m");
        assert_eq!(result, vec!["--system", ""]);
    }

    #[test]
    fn substitute_args_leaves_an_arg_with_no_placeholder_untouched() {
        let args = vec!["--quiet".to_string()];
        assert_eq!(substitute_args(&args, None, "m"), vec!["--quiet"]);
    }

    #[test]
    fn substitute_args_handles_multiple_placeholders_in_one_arg() {
        let args = vec!["{{model}}:{{model}}".to_string()];
        assert_eq!(substitute_args(&args, None, "m"), vec!["m:m"]);
    }

    #[test]
    fn substitute_args_handles_model_and_system_co_occurring_across_args() {
        let args = vec![
            "-m".to_string(),
            "{{model}}".to_string(),
            "--system".to_string(),
            "{{system}}".to_string(),
        ];
        let result = substitute_args(&args, Some("You are helpful."), "gpt-5.4");
        assert_eq!(
            result,
            vec!["-m", "gpt-5.4", "--system", "You are helpful."]
        );
    }
}
