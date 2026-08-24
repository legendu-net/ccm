//! `$EDITOR` resolution (prd.md, "Default behavior" under CLI Interface). Pure aside
//! from the injected [`Environment`] (var lookup + `$PATH` search), so no real
//! subprocess or filesystem is needed to test the splitting/fallback logic itself.

use crate::env::Environment;
use crate::error::EditorError;

/// The editor `ccm` will actually invoke: `<program> <leading_args...> <tempfile>`,
/// the temp file path always appended last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorPlan {
    pub program: String,
    pub leading_args: Vec<String>,
}

/// Resolves `$EDITOR` into an [`EditorPlan`]: word-split via `shell_words::split()`
/// (matching the class of splitting `git` applies to `GIT_EDITOR`/`core.editor`), first
/// word is the program, the rest are leading arguments. `$EDITOR` unset, empty, or
/// all-whitespace (splits to zero words) falls back in order to `nvim`, then `vim`,
/// then `vi` — whichever is found first on `$PATH`.
///
/// # Errors
/// [`EditorError::Unavailable`] (exit 12) if `$EDITOR` is set but its first word
/// doesn't resolve to an executable (a typo or a moved binary is *not* a fallback
/// trigger), or if `$EDITOR` is unset/blank and none of `nvim`/`vim`/`vi` are found.
pub fn plan_editor(env: &dyn Environment) -> Result<EditorPlan, EditorError> {
    if let Some(value) = env.var("EDITOR") {
        let words = shell_words::split(&value).map_err(|err| {
            EditorError::Unavailable(format!("failed to parse $EDITOR ({value:?}): {err}"))
        })?;
        if let Some((program, leading_args)) = words.split_first() {
            return if env.find_program(program).is_some() {
                Ok(EditorPlan {
                    program: program.clone(),
                    leading_args: leading_args.to_vec(),
                })
            } else {
                Err(EditorError::Unavailable(format!(
                    "$EDITOR is set to {value:?}, but {program:?} does not resolve to an executable"
                )))
            };
        }
        // Empty or all-whitespace: no program name to resolve at all, so this is
        // treated the same as $EDITOR being unset.
    }

    for candidate in ["nvim", "vim", "vi"] {
        if env.find_program(candidate).is_some() {
            return Ok(EditorPlan {
                program: candidate.to_string(),
                leading_args: vec![],
            });
        }
    }

    Err(EditorError::Unavailable(
        "$EDITOR is unset and none of nvim/vim/vi were found on $PATH".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::FakeEnvironment;

    #[test]
    fn splits_editor_with_a_leading_flag() {
        let env = FakeEnvironment::new()
            .with_var("EDITOR", "code --wait")
            .with_program("code", "/usr/bin/code");
        let plan = plan_editor(&env).unwrap();
        assert_eq!(plan.program, "code");
        assert_eq!(plan.leading_args, vec!["--wait".to_string()]);
    }

    #[test]
    fn splits_a_quoted_path_containing_spaces_as_one_word() {
        let env = FakeEnvironment::new()
            .with_var("EDITOR", "\"/p/with space/ed\" -x")
            .with_program("/p/with space/ed", "/p/with space/ed");
        let plan = plan_editor(&env).unwrap();
        assert_eq!(plan.program, "/p/with space/ed");
        assert_eq!(plan.leading_args, vec!["-x".to_string()]);
    }

    #[test]
    fn honors_backslash_escapes_outside_quotes() {
        let env = FakeEnvironment::new()
            .with_var("EDITOR", "ed\\ itor")
            .with_program("ed itor", "/usr/bin/ed itor");
        let plan = plan_editor(&env).unwrap();
        assert_eq!(plan.program, "ed itor");
    }

    #[test]
    fn empty_editor_falls_back_to_nvim() {
        let env = FakeEnvironment::new()
            .with_var("EDITOR", "")
            .with_program("nvim", "/usr/bin/nvim");
        let plan = plan_editor(&env).unwrap();
        assert_eq!(plan.program, "nvim");
        assert!(plan.leading_args.is_empty());
    }

    #[test]
    fn whitespace_only_editor_falls_back() {
        let env = FakeEnvironment::new()
            .with_var("EDITOR", "   ")
            .with_program("vim", "/usr/bin/vim");
        let plan = plan_editor(&env).unwrap();
        assert_eq!(plan.program, "vim");
    }

    #[test]
    fn unset_editor_falls_back_in_order_nvim_then_vim_then_vi() {
        let env = FakeEnvironment::new()
            .with_program("vim", "/usr/bin/vim")
            .with_program("vi", "/usr/bin/vi");
        // nvim not registered -> should skip to vim.
        let plan = plan_editor(&env).unwrap();
        assert_eq!(plan.program, "vim");
    }

    #[test]
    fn unset_editor_falls_back_to_vi_when_nothing_else_found() {
        let env = FakeEnvironment::new().with_program("vi", "/usr/bin/vi");
        let plan = plan_editor(&env).unwrap();
        assert_eq!(plan.program, "vi");
    }

    #[test]
    fn no_editor_and_no_fallback_found_is_exit_12() {
        let env = FakeEnvironment::new();
        assert!(matches!(
            plan_editor(&env),
            Err(EditorError::Unavailable(_))
        ));
    }

    #[test]
    fn editor_set_but_unresolvable_is_exit_12_not_a_fallback_trigger() {
        let env = FakeEnvironment::new()
            .with_var("EDITOR", "totally-not-a-real-editor")
            .with_program("nvim", "/usr/bin/nvim"); // present, but must NOT be used
        assert!(matches!(
            plan_editor(&env),
            Err(EditorError::Unavailable(_))
        ));
    }
}
