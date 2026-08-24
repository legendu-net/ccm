//! Creating and reading back the `$EDITOR` temp file (prd.md, "Default behavior" and
//! "Preferences of Dependencies" #6). The only I/O in the `editor` module's setup path.

use crate::error::EditorError;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Creates a uniquely-named temp file directly in `dir` (never inside the git/jj
/// repository), writes `content` into it, then disarms `tempfile`'s delete-on-drop —
/// the file must survive process exit unconditionally, on every exit path, so the user
/// always has a copy of what was generated/edited to recover from.
///
/// # Errors
/// [`EditorError::TempFile`] (exit 17) if creating the file or writing its content
/// fails (e.g. the OS temp directory is unwritable or the disk is full).
pub fn create_and_populate(dir: &Path, content: &str) -> Result<PathBuf, EditorError> {
    let mut file = tempfile::Builder::new()
        .prefix("CCM_EDITMSG_")
        .tempfile_in(dir)
        .map_err(|err| EditorError::TempFile(format!("failed to create temp file: {err}")))?;

    file.write_all(content.as_bytes())
        .map_err(|err| EditorError::TempFile(format!("failed to write temp file: {err}")))?;
    file.flush()
        .map_err(|err| EditorError::TempFile(format!("failed to write temp file: {err}")))?;

    file.into_temp_path()
        .keep()
        .map_err(|err| EditorError::TempFile(format!("failed to persist temp file: {err}")))
}

/// Reads the file `$EDITOR` saved back. Returns `None` if it's gone or unreadable by
/// now — the caller maps that to the generic exit-1 catch-all, since neither "editor
/// aborted" (13) nor "blank message" (15) applies to this specific gap.
#[must_use]
pub fn read_back(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_a_file_with_the_ccm_editmsg_prefix_and_the_given_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = create_and_populate(dir.path(), "hello\n").unwrap();
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("CCM_EDITMSG_")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    }

    #[test]
    fn the_file_survives_after_this_function_returns_no_drop_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let path = create_and_populate(dir.path(), "x").unwrap();
        // No explicit persistence step beyond what create_and_populate already did;
        // simply still being on disk here is the assertion.
        assert!(path.is_file());
    }

    #[test]
    fn fails_with_temp_file_error_when_the_directory_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let err = create_and_populate(&missing, "x").unwrap_err();
        assert!(matches!(err, EditorError::TempFile(_)));
    }

    #[test]
    fn fails_with_temp_file_error_when_the_directory_is_actually_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("file.txt");
        std::fs::write(&not_a_dir, "x").unwrap();
        let err = create_and_populate(&not_a_dir, "x").unwrap_err();
        assert!(matches!(err, EditorError::TempFile(_)));
    }

    #[test]
    fn read_back_returns_none_when_the_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_back(&dir.path().join("nope")), None);
    }

    #[test]
    fn read_back_returns_the_saved_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = create_and_populate(dir.path(), "original").unwrap();
        std::fs::write(&path, "edited content").unwrap();
        assert_eq!(read_back(&path), Some("edited content".to_string()));
    }
}
