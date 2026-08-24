//! Parsing `jj diff --summary` output (prd.md, "Diff scope resolution").
//!
//! One entry per line, not a deduplicated set of bare path strings — the rename/copy
//! handling below is why that distinction matters. A plain modification/addition/
//! deletion (`M`/`A`/`D`) line is the status character, a space, then the one path
//! (the whole line remainder). A rename/copy (`R`/`C`) line factors out the longest
//! common leading path the old and new names share and shows only the differing
//! suffixes in braces: `R <prefix>{<old-suffix> => <new-suffix>}`, e.g.
//! `R src/{old.rs => new.rs}`, or `R {a.txt => sub/b.txt}` when there's no shared
//! prefix at all (`<prefix>` then empty, line starts directly with `{`).

/// One parsed `jj diff --summary` line. `source` is `Some(<old>)` only for a rename/
/// copy (`R`/`C`); `target` is the one path for `M`/`A`/`D`, or `<new>` for `R`/`C` —
/// `<old>` is never a usable path to diff (renamed away, or an unchanged copy source),
/// so `target` alone is what a caller should treat as "the current path this line is
/// about".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryEntry {
    pub status: char,
    pub source: Option<String>,
    pub target: String,
}

/// A `jj diff --summary` line that didn't parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed jj diff --summary line: {line:?}")]
pub struct SummaryParseError {
    pub line: String,
}

const RENAME_OR_COPY: [char; 2] = ['R', 'C'];
const KNOWN_STATUSES: [char; 5] = ['M', 'A', 'D', 'R', 'C'];

/// Parses every non-empty line of `jj diff --summary` output.
///
/// # Errors
/// The first line that doesn't parse.
pub fn parse_summary(text: &str) -> Result<Vec<SummaryEntry>, SummaryParseError> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(parse_summary_line)
        .collect()
}

/// Parses one `jj diff --summary` line.
///
/// # Errors
/// If the line is shorter than "X path", has an unrecognized status character, or (for
/// `R`/`C`) has no `{`, doesn't end in `}`, or has no literal `" => "` inside the
/// braces.
pub fn parse_summary_line(line: &str) -> Result<SummaryEntry, SummaryParseError> {
    let malformed = || SummaryParseError {
        line: line.to_string(),
    };
    let mut chars = line.chars();
    let status = chars.next().ok_or_else(malformed)?;
    if !KNOWN_STATUSES.contains(&status) {
        return Err(malformed());
    }
    let rest = &line[status.len_utf8()..];
    let rest = rest.strip_prefix(' ').ok_or_else(malformed)?;

    if RENAME_OR_COPY.contains(&status) {
        parse_braced(status, rest, malformed)
    } else {
        Ok(SummaryEntry {
            status,
            source: None,
            target: rest.to_string(),
        })
    }
}

fn parse_braced(
    status: char,
    rest: &str,
    malformed: impl Fn() -> SummaryParseError,
) -> Result<SummaryEntry, SummaryParseError> {
    let brace_start = rest.find('{').ok_or_else(&malformed)?;
    if !rest.ends_with('}') {
        return Err(malformed());
    }
    let prefix = &rest[..brace_start];
    let inner = &rest[brace_start + 1..rest.len() - 1];
    let (old_suffix, new_suffix) = inner.split_once(" => ").ok_or_else(&malformed)?;
    Ok(SummaryEntry {
        status,
        source: Some(format!("{prefix}{old_suffix}")),
        target: format!("{prefix}{new_suffix}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_modified_line() {
        let entry = parse_summary_line("M src/main.rs").unwrap();
        assert_eq!(entry.status, 'M');
        assert_eq!(entry.source, None);
        assert_eq!(entry.target, "src/main.rs");
    }

    #[test]
    fn parses_an_added_line_with_a_nested_path() {
        let entry = parse_summary_line("A x/y.rs").unwrap();
        assert_eq!(entry.status, 'A');
        assert_eq!(entry.target, "x/y.rs");
    }

    #[test]
    fn parses_a_deleted_line() {
        let entry = parse_summary_line("D z").unwrap();
        assert_eq!(entry.status, 'D');
        assert_eq!(entry.target, "z");
    }

    #[test]
    fn parses_a_path_containing_spaces() {
        let entry = parse_summary_line("M my file.rs").unwrap();
        assert_eq!(entry.target, "my file.rs");
    }

    #[test]
    fn parses_a_rename_with_a_shared_prefix() {
        let entry = parse_summary_line("R src/{old.rs => new.rs}").unwrap();
        assert_eq!(entry.status, 'R');
        assert_eq!(entry.source.as_deref(), Some("src/old.rs"));
        assert_eq!(entry.target, "src/new.rs");
    }

    #[test]
    fn parses_a_copy_with_no_shared_prefix() {
        let entry = parse_summary_line("C {a.txt => sub/b.txt}").unwrap();
        assert_eq!(entry.status, 'C');
        assert_eq!(entry.source.as_deref(), Some("a.txt"));
        assert_eq!(entry.target, "sub/b.txt");
    }

    #[test]
    fn a_rename_line_ending_in_a_closing_brace_is_the_final_character() {
        let entry = parse_summary_line("R a/{x.rs => y{1}.rs}").unwrap();
        assert_eq!(entry.source.as_deref(), Some("a/x.rs"));
        assert_eq!(entry.target, "a/y{1}.rs");
    }

    #[test]
    fn rejects_an_empty_line() {
        assert!(parse_summary_line("").is_err());
    }

    #[test]
    fn rejects_an_unrecognized_status_character() {
        assert!(parse_summary_line("X foo.rs").is_err());
    }

    #[test]
    fn rejects_a_status_with_no_following_space() {
        assert!(parse_summary_line("Mfoo.rs").is_err());
    }

    #[test]
    fn rejects_a_rename_line_with_no_brace() {
        assert!(parse_summary_line("R old.rs => new.rs").is_err());
    }

    #[test]
    fn rejects_a_rename_line_not_ending_in_a_brace() {
        assert!(parse_summary_line("R src/{old.rs => new.rs} ").is_err());
    }

    #[test]
    fn rejects_a_rename_line_with_no_arrow_separator() {
        assert!(parse_summary_line("R src/{old.rs, new.rs}").is_err());
    }

    #[test]
    fn parses_multiple_lines_in_order() {
        let entries = parse_summary("M a.rs\nA b.rs\nR c/{old.rs => new.rs}\n").unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].status, 'M');
        assert_eq!(entries[1].status, 'A');
        assert_eq!(entries[2].status, 'R');
    }

    #[test]
    fn ignores_trailing_blank_lines() {
        let entries = parse_summary("M a.rs\n\n").unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn empty_input_parses_to_no_entries() {
        assert_eq!(parse_summary("").unwrap(), vec![]);
    }
}
