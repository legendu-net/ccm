//! Stage 8 (default-mode half), up to but not including the jj commit-command picker
//! and the final commit invocation (that's `picker.rs`, wired in by the next phase):
//! resolve `$EDITOR`, create and pre-populate its temp file, run it (inheriting the
//! real controlling terminal — this is the one place `ccm` spawns a subprocess without
//! going through `vcs::exec`, which captures stdio instead of inheriting it), read
//! back and clean up what it saved, and check for blank.

pub mod message;
pub mod resolve;
pub mod tempmsg;

use crate::env::Environment;
use crate::error::{CcmError, EditorError};
use std::process::Command;

/// Runs the editor flow to a cleaned, non-blank message.
///
/// # Errors
/// [`EditorError::Unavailable`] (12), [`EditorError::TempFile`] (17),
/// [`EditorError::Aborted`] (13), [`CcmError::Unexpected`] (1, if the saved file goes
/// missing between the editor closing and `ccm` reading it back), or
/// [`EditorError::BlankMessage`] (15).
pub fn edit_message(generated: Option<&str>, env: &dyn Environment) -> Result<String, CcmError> {
    let plan = resolve::plan_editor(env)?;
    let content = message::prepopulate(generated);
    let temp_path = tempmsg::create_and_populate(&env.temp_dir(), &content)?;

    let status = Command::new(&plan.program)
        .args(&plan.leading_args)
        .arg(&temp_path)
        .status()
        .map_err(|err| CcmError::Unexpected(format!("failed to run editor: {err}")))?;

    if !status.success() {
        return Err(EditorError::Aborted.into());
    }

    let saved = tempmsg::read_back(&temp_path).ok_or_else(|| {
        CcmError::Unexpected(format!(
            "editor exited successfully but the temp file {} is missing or unreadable",
            temp_path.display()
        ))
    })?;

    let cleaned = message::cleanup(&saved);
    if message::is_blank(&cleaned) {
        return Err(EditorError::BlankMessage.into());
    }
    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::FakeEnvironment;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn fake_editor(dir: &std::path::Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path.to_str().unwrap().to_string()
    }

    fn env_with_editor(temp_dir: &std::path::Path, editor_path: &str) -> FakeEnvironment {
        FakeEnvironment {
            temp_dir: temp_dir.to_path_buf(),
            ..FakeEnvironment::new()
        }
        .with_var("EDITOR", editor_path)
        .with_program(editor_path, editor_path)
    }

    /// `edit_message`, tolerant of a transient `ETXTBSY` ("Text file busy") when
    /// executing a script this same test just wrote and `chmod`'d — an environment
    /// artifact of many test threads writing+exec'ing fresh files concurrently on some
    /// filesystems (confirmed non-deterministic: the same test passes reliably in
    /// isolation), not a real `ccm` bug. `edit_message` creates a fresh temp file on
    /// every call, so retrying is safe regardless of what the fake editor does to it.
    fn edit_message_retrying(
        generated: Option<&str>,
        env: &dyn Environment,
    ) -> Result<String, CcmError> {
        for attempt in 0..5 {
            match edit_message(generated, env) {
                Err(CcmError::Unexpected(msg)) if msg.contains("Text file busy") => {
                    assert!(attempt < 4, "editor still busy after retries: {msg}");
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                result => return result,
            }
        }
        unreachable!()
    }

    #[test]
    fn a_saving_editor_produces_the_cleaned_message() {
        let scratch = tempfile::tempdir().unwrap();
        let editor = fake_editor(scratch.path(), "ed", "printf 'feat: x\\n' > \"$1\"");
        let env = env_with_editor(scratch.path(), &editor);
        let message = edit_message_retrying(Some("generated"), &env).unwrap();
        assert_eq!(message, "feat: x");
    }

    #[test]
    fn an_editor_that_does_nothing_keeps_the_prepopulated_message() {
        let scratch = tempfile::tempdir().unwrap();
        let editor = fake_editor(scratch.path(), "ed", "exit 0");
        let env = env_with_editor(scratch.path(), &editor);
        let message = edit_message_retrying(Some("generated message"), &env).unwrap();
        assert_eq!(message, "generated message");
    }

    #[test]
    fn a_failing_editor_is_aborted_exit_13() {
        let scratch = tempfile::tempdir().unwrap();
        let editor = fake_editor(scratch.path(), "ed", "exit 3");
        let env = env_with_editor(scratch.path(), &editor);
        let err = edit_message_retrying(Some("generated"), &env).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 13);
    }

    #[test]
    fn a_blank_saved_file_is_exit_15() {
        let scratch = tempfile::tempdir().unwrap();
        let editor = fake_editor(scratch.path(), "ed", ": > \"$1\"");
        let env = env_with_editor(scratch.path(), &editor);
        let err = edit_message_retrying(None, &env).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 15);
    }

    #[test]
    fn an_editor_that_deletes_the_file_is_the_exit_1_catch_all() {
        let scratch = tempfile::tempdir().unwrap();
        let editor = fake_editor(scratch.path(), "ed", "rm \"$1\"");
        let env = env_with_editor(scratch.path(), &editor);
        let err = edit_message_retrying(Some("generated"), &env).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 1);
    }

    #[test]
    fn no_usable_editor_is_exit_12() {
        let scratch = tempfile::tempdir().unwrap();
        let env = FakeEnvironment {
            temp_dir: scratch.path().to_path_buf(),
            ..FakeEnvironment::new()
        };
        let err = edit_message(Some("generated"), &env).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 12);
    }
}
