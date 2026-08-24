//! Prompt rendering: the resolved `prompts.yaml` entry, the `openai_api` user message,
//! and `{{prompt}}`/`{{model}}`/`{{system}}` substitution for `agent_cli` `args`
//! (prd.md, `prompts.yaml` and `type: agent_cli` fields under Configuration). Pure.

/// A resolved `prompts.yaml` entry, borrowed for the duration of one generation call.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedPrompt<'a> {
    pub system: Option<&'a str>,
    pub template: &'a str,
}

/// The `openai_api` user-role message: `template` verbatim (no trimming of its own
/// trailing whitespace), a `\n\n` separator, then the diff verbatim — no `Diff:` label,
/// since the diff itself already makes clear where it starts.
#[must_use]
pub fn openai_user_message(template: &str, diff: &str) -> String {
    format!("{template}\n\n{diff}")
}

/// Substitutes `{{prompt}}` (the prompt's `template`, verbatim — never the diff),
/// `{{model}}`, and `{{system}}` (the prompt's `system`, or an empty string if unset)
/// into each `agent_cli` `args` entry. An arg with no placeholder is returned
/// unchanged; an arg can contain more than one placeholder.
#[must_use]
pub fn substitute_args(args: &[String], prompt: &ResolvedPrompt<'_>, model: &str) -> Vec<String> {
    args.iter()
        .map(|arg| substitute_one(arg, prompt, model))
        .collect()
}

/// A single left-to-right pass over `arg`, rather than three sequential
/// `str::replace` calls: sequential whole-string replacement would re-scan text a
/// prior substitution just inserted, so a `template` (inserted verbatim for
/// `{{prompt}}`) that itself happens to contain the literal substring `{{model}}` or
/// `{{system}}` would get corrupted by the later replacement passes. Scanning once and
/// advancing past each match (never revisiting already-emitted output) keeps every
/// substituted value's own text untouched.
fn substitute_one(arg: &str, prompt: &ResolvedPrompt<'_>, model: &str) -> String {
    const PLACEHOLDERS: [&str; 3] = ["{{prompt}}", "{{model}}", "{{system}}"];
    let mut result = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some((offset, token)) = PLACEHOLDERS
        .iter()
        .filter_map(|token| rest.find(token).map(|offset| (offset, *token)))
        .min_by_key(|(offset, _)| *offset)
    {
        let value = match token {
            "{{prompt}}" => prompt.template,
            "{{model}}" => model,
            _ => prompt.system.unwrap_or(""),
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
    fn openai_user_message_joins_template_and_diff_with_a_blank_line() {
        assert_eq!(
            openai_user_message("Write a commit message.", "diff --git a/x b/x"),
            "Write a commit message.\n\ndiff --git a/x b/x"
        );
    }

    #[test]
    fn openai_user_message_does_not_trim_template_trailing_whitespace() {
        assert_eq!(
            openai_user_message("Write it.  ", "DIFF"),
            "Write it.  \n\nDIFF"
        );
    }

    #[test]
    fn a_template_containing_literal_placeholder_syntax_survives_intact() {
        // Regression test: sequential whole-string .replace() calls would let a later
        // pass re-scan and corrupt text a prior pass just inserted. A template that
        // itself mentions "{{model}}" (e.g. a meta-prompt about ccm's own syntax) must
        // come through byte-for-byte once substituted for {{prompt}}.
        let prompt = ResolvedPrompt {
            system: None,
            template: "Explain what {{model}} means in this context.",
        };
        let args = vec!["-p".to_string(), "{{prompt}}".to_string()];
        let result = substitute_args(&args, &prompt, "gpt-5.4");
        assert_eq!(
            result,
            vec!["-p", "Explain what {{model}} means in this context."]
        );
    }

    #[test]
    fn substitute_args_replaces_prompt_and_model() {
        let prompt = ResolvedPrompt {
            system: None,
            template: "write a message",
        };
        let args = vec![
            "-m".to_string(),
            "{{model}}".to_string(),
            "-p".to_string(),
            "{{prompt}}".to_string(),
        ];
        let result = substitute_args(&args, &prompt, "gemini-2.5-pro");
        assert_eq!(
            result,
            vec!["-m", "gemini-2.5-pro", "-p", "write a message"]
        );
    }

    #[test]
    fn substitute_args_fills_system_when_present() {
        let prompt = ResolvedPrompt {
            system: Some("You are helpful."),
            template: "write it",
        };
        let args = vec!["--system".to_string(), "{{system}}".to_string()];
        let result = substitute_args(&args, &prompt, "m");
        assert_eq!(result, vec!["--system", "You are helpful."]);
    }

    #[test]
    fn substitute_args_uses_empty_string_when_system_placeholder_present_but_unset() {
        let prompt = ResolvedPrompt {
            system: None,
            template: "write it",
        };
        let args = vec!["--system".to_string(), "{{system}}".to_string()];
        let result = substitute_args(&args, &prompt, "m");
        assert_eq!(result, vec!["--system", ""]);
    }

    #[test]
    fn substitute_args_leaves_an_arg_with_no_placeholder_untouched() {
        let prompt = ResolvedPrompt {
            system: None,
            template: "write it",
        };
        let args = vec!["--quiet".to_string()];
        assert_eq!(substitute_args(&args, &prompt, "m"), vec!["--quiet"]);
    }

    #[test]
    fn substitute_args_handles_multiple_placeholders_in_one_arg() {
        let prompt = ResolvedPrompt {
            system: None,
            template: "T",
        };
        let args = vec!["{{model}}:{{model}}".to_string()];
        assert_eq!(substitute_args(&args, &prompt, "m"), vec!["m:m"]);
    }
}
