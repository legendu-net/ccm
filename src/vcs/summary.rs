//! Parsing the jj diff enumeration call's output (prd.md, "Diff scope resolution").
//!
//! `argv::jj_enumerate_args`'s `-T` template renders one
//! `<status-char><SEP><source-path><SEP><target-path>` record per line (`\n`-terminated,
//! `SEP` = [`argv::ENUMERATE_FIELD_SEP`]), so parsing a line here is a plain split on
//! that delimiter — no ambiguity, ever, since no real path can contain it.
//!
//! This replaces an earlier approach that instead ran `jj diff --summary` and parsed
//! its human-oriented output, where a rename/copy line shows `<old>`/`<new>` factored
//! into a shared prefix/suffix with the differing middle in braces (e.g. `R
//! src/{old.rs => new.rs}`). That notation turned out to be genuinely ambiguous to
//! recover in several real cases — jj does not escape a literal `{`/`}` a filename
//! itself contains, so no amount of smarter brace-matching heuristics (bracket-depth
//! tracking, then boundary-adjacency to a path separator) fully closed the gap; each
//! fix in turn was defeated by a new real example. The template-based format sidesteps
//! that whole class of problem instead of continuing to out-clever it.
//!
//! One entry per line, not a deduplicated set of bare path strings — the rename/copy
//! handling below is why that distinction matters (see `vcs::scope`'s doc comment for
//! how `source`/`target` are used downstream). jj's template emits `source`/`target` as
//! equal strings for a plain modification/addition/deletion (`M`/`A`/`D`) line, not just
//! for a rename/copy (`R`/`C`) — `ccm` discards the redundant `source` for those
//! statuses rather than exposing it, since only a rename/copy's old name is ever
//! independently useful downstream (see [`SummaryEntry`]'s doc comment).

use super::argv::ENUMERATE_FIELD_SEP;

/// One parsed enumeration-line entry. `source` is `Some(<old>)` only for a rename/copy
/// (`R`/`C`); `target` is the one path for `M`/`A`/`D`, or `<new>` for `R`/`C` — `<old>`
/// is never a usable path to diff (renamed away, or an unchanged copy source), so
/// `target` alone is what a caller should treat as "the current path this line is
/// about".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryEntry {
    pub status: char,
    pub source: Option<String>,
    pub target: String,
}

/// An enumeration line that didn't parse. In practice this means the `-T` template in
/// `argv::jj_enumerate_args` and the parsing here have drifted out of sync (or jj
/// changed `status_char`'s possible values) — not something a real working copy's file
/// paths can trigger, unlike the old `--summary`-parsing approach's brace ambiguity.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed jj diff enumeration line: {line:?}")]
pub struct SummaryParseError {
    pub line: String,
}

const RENAME_OR_COPY: [char; 2] = ['R', 'C'];
const KNOWN_STATUSES: [char; 5] = ['M', 'A', 'D', 'R', 'C'];

/// Parses every non-empty line of the enumeration call's output.
///
/// # Errors
/// The first line that doesn't parse.
pub fn parse_summary(text: &str) -> Result<Vec<SummaryEntry>, SummaryParseError> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(parse_summary_line)
        .collect()
}

/// Parses one enumeration line: `<status><SEP><source><SEP><target>`.
///
/// # Errors
/// If the line doesn't split into exactly three `SEP`-delimited fields, if the source or
/// target field is empty, or if the first field isn't a single recognized status
/// character (`M`/`A`/`D`/`R`/`C`).
pub fn parse_summary_line(line: &str) -> Result<SummaryEntry, SummaryParseError> {
    let malformed = || SummaryParseError {
        line: line.to_string(),
    };
    let mut fields = line.split(ENUMERATE_FIELD_SEP);
    let (Some(status_field), Some(source), Some(target), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(malformed());
    };
    if source.is_empty() || target.is_empty() {
        return Err(malformed());
    }

    let mut status_chars = status_field.chars();
    let status = status_chars.next().ok_or_else(malformed)?;
    if status_chars.next().is_some() || !KNOWN_STATUSES.contains(&status) {
        return Err(malformed());
    }

    Ok(SummaryEntry {
        status,
        source: RENAME_OR_COPY.contains(&status).then(|| source.to_string()),
        target: target.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_line(status: char, source: &str, target: &str) -> String {
        format!("{status}{ENUMERATE_FIELD_SEP}{source}{ENUMERATE_FIELD_SEP}{target}")
    }

    #[test]
    fn parses_a_modified_line() {
        let entry = parse_summary_line(&make_line('M', "src/main.rs", "src/main.rs")).unwrap();
        assert_eq!(entry.status, 'M');
        assert_eq!(entry.source, None);
        assert_eq!(entry.target, "src/main.rs");
    }

    #[test]
    fn parses_an_added_line_with_a_nested_path() {
        let entry = parse_summary_line(&make_line('A', "x/y.rs", "x/y.rs")).unwrap();
        assert_eq!(entry.status, 'A');
        assert_eq!(entry.source, None);
        assert_eq!(entry.target, "x/y.rs");
    }

    #[test]
    fn parses_a_deleted_line() {
        let entry = parse_summary_line(&make_line('D', "z", "z")).unwrap();
        assert_eq!(entry.status, 'D');
        assert_eq!(entry.source, None);
        assert_eq!(entry.target, "z");
    }

    #[test]
    fn parses_a_path_containing_spaces() {
        let entry = parse_summary_line(&make_line('M', "my file.rs", "my file.rs")).unwrap();
        assert_eq!(entry.target, "my file.rs");
    }

    #[test]
    fn parses_a_rename() {
        let entry = parse_summary_line(&make_line('R', "src/old.rs", "src/new.rs")).unwrap();
        assert_eq!(entry.status, 'R');
        assert_eq!(entry.source.as_deref(), Some("src/old.rs"));
        assert_eq!(entry.target, "src/new.rs");
    }

    #[test]
    fn parses_a_copy() {
        let entry = parse_summary_line(&make_line('C', "a.txt", "sub/b.txt")).unwrap();
        assert_eq!(entry.status, 'C');
        assert_eq!(entry.source.as_deref(), Some("a.txt"));
        assert_eq!(entry.target, "sub/b.txt");
    }

    #[test]
    fn a_shared_suffix_rename_is_not_confused_with_a_shared_prefix() {
        // The real-world case that motivated this template-based format (prd.md "Diff
        // scope resolution"): the old `--summary` brace notation factored `<old>`/`<new>`
        // into a shared prefix/suffix (`.github/{workflow => workflows}/ci.yml`), which
        // was ambiguous to parse back when a filename itself could contain `{`/`}`. A
        // plain `SEP`-delimited split has no such ambiguity regardless of how much
        // prefix or suffix the two paths share.
        let entry = parse_summary_line(&make_line(
            'R',
            ".github/workflow/ci.yml",
            ".github/workflows/ci.yml",
        ))
        .unwrap();
        assert_eq!(entry.source.as_deref(), Some(".github/workflow/ci.yml"));
        assert_eq!(entry.target, ".github/workflows/ci.yml");
    }

    #[test]
    fn a_literal_brace_in_a_path_is_just_a_character_now() {
        // The exact case that broke the old `--summary`-based brace parser (a filename
        // literally containing an unescaped `}`) needs no special handling at all here.
        let entry = parse_summary_line(&make_line('R', "a/x.rs", "a/y}1.rs")).unwrap();
        assert_eq!(entry.source.as_deref(), Some("a/x.rs"));
        assert_eq!(entry.target, "a/y}1.rs");
    }

    #[test]
    fn a_multi_byte_utf8_path_parses_correctly() {
        // Field splitting is `str::split`/`str::chars`, never a raw byte index, so this
        // needs no special-casing either — unlike the old approach's `as_bytes()[idx]`
        // boundary checks, which had to be reasoned about carefully for char-boundary
        // safety.
        let entry = parse_summary_line(&make_line('R', "café/old.rs", "café/новый.rs")).unwrap();
        assert_eq!(entry.source.as_deref(), Some("café/old.rs"));
        assert_eq!(entry.target, "café/новый.rs");
    }

    #[test]
    fn rejects_an_empty_line() {
        assert!(parse_summary_line("").is_err());
    }

    #[test]
    fn rejects_too_few_fields() {
        assert!(parse_summary_line(&format!("M{ENUMERATE_FIELD_SEP}a.rs")).is_err());
    }

    #[test]
    fn rejects_too_many_fields() {
        let line = format!(
            "{}{ENUMERATE_FIELD_SEP}extra",
            make_line('M', "a.rs", "a.rs")
        );
        assert!(parse_summary_line(&line).is_err());
    }

    #[test]
    fn rejects_an_empty_source_field() {
        assert!(parse_summary_line(&make_line('R', "", "new.rs")).is_err());
    }

    #[test]
    fn rejects_an_empty_target_field() {
        assert!(parse_summary_line(&make_line('M', "a.rs", "")).is_err());
    }

    #[test]
    fn rejects_an_unrecognized_status_character() {
        assert!(parse_summary_line(&make_line('X', "foo.rs", "foo.rs")).is_err());
    }

    #[test]
    fn rejects_a_multi_character_status_field() {
        let line = format!("MM{ENUMERATE_FIELD_SEP}foo.rs{ENUMERATE_FIELD_SEP}foo.rs");
        assert!(parse_summary_line(&line).is_err());
    }

    #[test]
    fn parses_multiple_lines_in_order() {
        let text = format!(
            "{}\n{}\n{}\n",
            make_line('M', "a.rs", "a.rs"),
            make_line('A', "b.rs", "b.rs"),
            make_line('R', "c/old.rs", "c/new.rs"),
        );
        let entries = parse_summary(&text).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].status, 'M');
        assert_eq!(entries[1].status, 'A');
        assert_eq!(entries[2].status, 'R');
    }

    #[test]
    fn ignores_trailing_blank_lines() {
        let text = format!("{}\n\n", make_line('M', "a.rs", "a.rs"));
        let entries = parse_summary(&text).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn empty_input_parses_to_no_entries() {
        assert_eq!(parse_summary("").unwrap(), vec![]);
    }
}
