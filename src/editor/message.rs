//! Pre-populating the `$EDITOR` temp file and cleaning up what it saves (prd.md,
//! "Message pre-population and cleanup"). Pure.

/// `#CCM: ` comment lines strip on the literal prefix (not a bare `#`, git-style).
const CCM_COMMENT_PREFIX: &str = "#CCM: ";

/// Builds the temp file's initial content: the generated message (if any) followed by
/// a blank line and a `#CCM: `-prefixed comment block explaining what to do.
#[must_use]
pub fn prepopulate(generated: Option<&str>) -> String {
    match generated {
        Some(message) => format!(
            "{message}\n\n{CCM_COMMENT_PREFIX}message generated above. Edit as needed, save, and exit to commit.\n"
        ),
        None => format!(
            "\n{CCM_COMMENT_PREFIX}no message was generated (the tool/API returned a blank response).\n{CCM_COMMENT_PREFIX}Write your own commit message above, or leave this blank to abort.\n"
        ),
    }
}

/// Cleans up the file `$EDITOR` saved: lines starting with the literal `#CCM: ` prefix
/// are removed, then leading and trailing blank lines are trimmed from what remains —
/// a bare `#` is not a comment marker (a line like `#123 fixes the thing` or a markdown
/// heading is left alone), and a blank line in the *middle* of an otherwise non-blank
/// message is left alone too.
#[must_use]
pub fn cleanup(saved: &str) -> String {
    let kept: Vec<&str> = saved
        .lines()
        .filter(|line| !line.starts_with(CCM_COMMENT_PREFIX))
        .collect();

    let is_blank_line = |line: &&str| line.trim().is_empty();
    let Some(start) = kept.iter().position(|line| !is_blank_line(line)) else {
        return String::new();
    };
    let end = kept.iter().rposition(|line| !is_blank_line(line)).unwrap();
    kept[start..=end].join("\n")
}

/// Whether `s` is blank — empty or all whitespace (prd.md exit code 15).
#[must_use]
pub fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepopulate_with_a_generated_message() {
        let text = prepopulate(Some("feat: add x"));
        assert_eq!(
            text,
            "feat: add x\n\n#CCM: message generated above. Edit as needed, save, and exit to commit.\n"
        );
    }

    #[test]
    fn prepopulate_with_no_message() {
        let text = prepopulate(None);
        assert_eq!(
            text,
            "\n#CCM: no message was generated (the tool/API returned a blank response).\n#CCM: Write your own commit message above, or leave this blank to abort.\n"
        );
    }

    #[test]
    fn cleanup_strips_ccm_comment_lines() {
        let saved = "feat: add x\n\n#CCM: message generated above.\n";
        assert_eq!(cleanup(saved), "feat: add x");
    }

    #[test]
    fn cleanup_leaves_a_bare_hash_line_alone() {
        let saved = "#123 fixes the thing\n\n#CCM: comment\n";
        assert_eq!(cleanup(saved), "#123 fixes the thing");
    }

    #[test]
    fn cleanup_leaves_a_markdown_heading_alone() {
        let saved = "# Heading\n\nbody\n";
        assert_eq!(cleanup(saved), "# Heading\n\nbody");
    }

    #[test]
    fn cleanup_leaves_interior_blank_lines_alone() {
        let saved = "line one\n\nline two\n";
        assert_eq!(cleanup(saved), "line one\n\nline two");
    }

    #[test]
    fn cleanup_trims_leading_and_trailing_blank_lines_only() {
        let saved = "\n\nfeat: x\n\n\n";
        assert_eq!(cleanup(saved), "feat: x");
    }

    #[test]
    fn cleanup_of_an_all_comment_file_is_blank() {
        let saved = "#CCM: line one\n#CCM: line two\n";
        assert_eq!(cleanup(saved), "");
    }

    #[test]
    fn cleanup_of_a_bare_ccm_with_no_trailing_space_is_left_alone() {
        // The match is on the literal "#CCM: " prefix (with trailing space); "#CCM:"
        // alone doesn't match it.
        let saved = "#CCM:no space after colon\n";
        assert_eq!(cleanup(saved), "#CCM:no space after colon");
    }

    #[test]
    fn is_blank_detects_empty_and_whitespace_only() {
        assert!(is_blank(""));
        assert!(is_blank("   \n\t\n"));
        assert!(!is_blank("feat: x"));
    }
}
