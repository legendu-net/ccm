//! Resolving `--include`/`--exclude` against parsed jj diff enumeration entries (prd.md,
//! "Diff scope resolution"). Pure — operates purely over already-parsed
//! [`SummaryEntry`] data.
//!
//! The "contribution" a line makes to any resulting file-argument list is always
//! `target` — the `M`/`A`/`D` path, or a rename/copy's `<new>` name (never `<old>`,
//! since `<old>` no longer exists to diff after a rename, and is unchanged by
//! definition after a copy). This holds uniformly across [`Selection::All`],
//! [`Selection::Include`], and [`Selection::Exclude`], which is what makes "the whole
//! working copy" (`All`) and "the whole working copy minus these" (`Exclude`) simple
//! variations on the same contribution rule rather than two different mechanisms.
//!
//! *Matching* (whether a user-given `--include`/`--exclude` pattern activates a given
//! line) is broader than contribution, though: a rename/copy line is matched by a
//! pattern naming *either* `<old>` or `<new>` — but still only ever contributes `<new>`
//! when matched.

use super::pathnorm::{self, NormalizedPath};
use super::summary::SummaryEntry;
use std::collections::HashSet;
use std::path::PathBuf;

/// What `--include`/`--exclude` (or neither) resolves to before scope resolution runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Neither flag was given: the entire working copy.
    All,
    /// `--include <patterns>`: only files matched by at least one pattern.
    Include(Vec<NormalizedPath>),
    /// `--exclude <patterns>`: every file except those matched by at least one pattern.
    Exclude(Vec<NormalizedPath>),
    /// Files already resolved by the interactive file picker (prd.md "Diff scope
    /// resolution") — no enumeration or pattern matching left to do; `resolve_scope`
    /// just deduplicates and returns them as-is.
    Explicit(Vec<String>),
}

impl Selection {
    /// Builds a [`Selection`] from the CLI's `--include`/`--exclude` values, which are
    /// already known to be mutually exclusive by this point (stage 1). Normalizes
    /// every pattern per `pathnorm::normalize`.
    #[must_use]
    pub fn from_cli(include: &[PathBuf], exclude: &[PathBuf]) -> Selection {
        if !include.is_empty() {
            Selection::Include(include.iter().map(|p| pathnorm::normalize(p)).collect())
        } else if !exclude.is_empty() {
            Selection::Exclude(exclude.iter().map(|p| pathnorm::normalize(p)).collect())
        } else {
            Selection::All
        }
    }
}

/// A `--include`/`--exclude` entry that matched zero lines of the enumerated working
/// copy (prd.md: "a usage error, not silently ignored").
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{entry}: does not match any file in the jj working copy")]
pub struct ScopeError {
    pub entry: String,
}

/// Resolves `selection` against the enumerated working-copy `entries`, returning the
/// (deduplicated, first-seen-order) list of paths to pass as `jj diff`/`jj commit`/
/// `jj split` file arguments.
///
/// # Errors
/// [`ScopeError`] if any `--include`/`--exclude` entry matches zero lines.
pub fn resolve_scope(
    entries: &[SummaryEntry],
    selection: &Selection,
) -> Result<Vec<String>, ScopeError> {
    match selection {
        Selection::All => Ok(dedup_preserve_order(
            entries.iter().map(|entry| entry.target.clone()),
        )),
        Selection::Include(patterns) => resolve_include(entries, patterns),
        Selection::Exclude(patterns) => resolve_exclude(entries, patterns),
        Selection::Explicit(files) => Ok(dedup_preserve_order(files.iter().cloned())),
    }
}

fn resolve_include(
    entries: &[SummaryEntry],
    patterns: &[NormalizedPath],
) -> Result<Vec<String>, ScopeError> {
    let mut pattern_matched = vec![false; patterns.len()];
    // Borrowed from `entries` (which outlives this function), so membership tracking
    // needs no clone at all — only the one clone per kept entry, when it's pushed.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut result = Vec::new();

    for entry in entries {
        let line_matches = line_matches_any_pattern(entry, patterns, &mut pattern_matched);
        if line_matches && seen.insert(&entry.target) {
            result.push(entry.target.clone());
        }
    }

    require_all_matched(patterns, &pattern_matched)?;
    Ok(result)
}

fn resolve_exclude(
    entries: &[SummaryEntry],
    patterns: &[NormalizedPath],
) -> Result<Vec<String>, ScopeError> {
    let mut pattern_matched = vec![false; patterns.len()];
    let mut excluded_targets: HashSet<&str> = HashSet::new();

    for entry in entries {
        if line_matches_any_pattern(entry, patterns, &mut pattern_matched) {
            excluded_targets.insert(&entry.target);
        }
    }

    require_all_matched(patterns, &pattern_matched)?;

    let mut seen: HashSet<&str> = HashSet::new();
    let mut result = Vec::new();
    for entry in entries {
        if !excluded_targets.contains(entry.target.as_str()) && seen.insert(&entry.target) {
            result.push(entry.target.clone());
        }
    }
    Ok(result)
}

/// Checks `entry`'s match candidates (`source` and `target` for a rename/copy, just
/// `target` otherwise) against every pattern, marking each pattern that matched in
/// `pattern_matched`. Returns whether *any* pattern matched this line.
fn line_matches_any_pattern(
    entry: &SummaryEntry,
    patterns: &[NormalizedPath],
    pattern_matched: &mut [bool],
) -> bool {
    let candidates: Vec<NormalizedPath> = match &entry.source {
        Some(source) => vec![
            pathnorm::normalize_str(source),
            pathnorm::normalize_str(&entry.target),
        ],
        None => vec![pathnorm::normalize_str(&entry.target)],
    };

    let mut line_matched = false;
    for (pattern, matched) in patterns.iter().zip(pattern_matched.iter_mut()) {
        if candidates
            .iter()
            .any(|candidate| pathnorm::matches(pattern, candidate))
        {
            *matched = true;
            line_matched = true;
        }
    }
    line_matched
}

fn require_all_matched(patterns: &[NormalizedPath], matched: &[bool]) -> Result<(), ScopeError> {
    if let Some(index) = matched.iter().position(|m| !m) {
        return Err(ScopeError {
            entry: patterns[index].to_string(),
        });
    }
    Ok(())
}

/// `pub(crate)` (rather than private) so `fileselect.rs` can reuse it for the
/// interactive file picker's candidate list, instead of duplicating it.
pub(crate) fn dedup_preserve_order(iter: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    iter.filter(|item| seen.insert(item.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(path: &str) -> SummaryEntry {
        SummaryEntry {
            status: 'M',
            source: None,
            target: path.to_string(),
        }
    }

    fn a(path: &str) -> SummaryEntry {
        SummaryEntry {
            status: 'A',
            source: None,
            target: path.to_string(),
        }
    }

    fn r(source: &str, target: &str) -> SummaryEntry {
        SummaryEntry {
            status: 'R',
            source: Some(source.to_string()),
            target: target.to_string(),
        }
    }

    fn c(source: &str, target: &str) -> SummaryEntry {
        SummaryEntry {
            status: 'C',
            source: Some(source.to_string()),
            target: target.to_string(),
        }
    }

    fn patterns(items: &[&str]) -> Vec<NormalizedPath> {
        items.iter().map(|s| pathnorm::normalize_str(s)).collect()
    }

    #[test]
    fn all_contributes_every_target_in_order() {
        let e = vec![m("a.rs"), a("b.rs"), r("c/old.rs", "c/new.rs")];
        let result = resolve_scope(&e, &Selection::All).unwrap();
        assert_eq!(result, vec!["a.rs", "b.rs", "c/new.rs"]);
    }

    #[test]
    fn explicit_returns_the_given_files_deduplicated_and_in_order() {
        // The interactive file picker (`fileselect.rs`) already resolves against
        // real enumerated entries before building this variant, so `resolve_scope`
        // itself ignores `entries` for it — pass an unrelated one to pin that.
        let e = vec![m("unrelated.rs")];
        let sel = Selection::Explicit(vec![
            "b.rs".to_string(),
            "a.rs".to_string(),
            "b.rs".to_string(),
        ]);
        let result = resolve_scope(&e, &sel).unwrap();
        assert_eq!(result, vec!["b.rs", "a.rs"]);
    }

    #[test]
    fn include_restricts_to_matched_files() {
        let e = vec![m("a.rs"), a("b.rs"), m("src/x.rs")];
        let sel = Selection::Include(patterns(&["a.rs"]));
        let result = resolve_scope(&e, &sel).unwrap();
        assert_eq!(result, vec!["a.rs"]);
    }

    #[test]
    fn include_a_directory_matches_every_nested_file() {
        let e = vec![m("src/a.rs"), m("src/sub/b.rs"), m("other.rs")];
        let sel = Selection::Include(patterns(&["src"]));
        let result = resolve_scope(&e, &sel).unwrap();
        assert_eq!(result, vec!["src/a.rs", "src/sub/b.rs"]);
    }

    #[test]
    fn include_an_unmatched_entry_is_a_scope_error() {
        let e = vec![m("a.rs")];
        let sel = Selection::Include(patterns(&["typo.rs"]));
        let err = resolve_scope(&e, &sel).unwrap_err();
        assert_eq!(err.entry, "typo.rs");
    }

    #[test]
    fn exclude_removes_matched_files_and_keeps_the_rest() {
        let e = vec![m("a.rs"), a("b.rs"), m("c.rs")];
        let sel = Selection::Exclude(patterns(&["b.rs"]));
        let result = resolve_scope(&e, &sel).unwrap();
        assert_eq!(result, vec!["a.rs", "c.rs"]);
    }

    #[test]
    fn exclude_everything_yields_an_empty_list_not_an_error() {
        let e = vec![m("a.rs"), a("b.rs")];
        let sel = Selection::Exclude(patterns(&["a.rs", "b.rs"]));
        let result = resolve_scope(&e, &sel).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn exclude_an_unmatched_entry_is_a_scope_error() {
        let e = vec![m("a.rs")];
        let sel = Selection::Exclude(patterns(&["typo.rs"]));
        assert!(resolve_scope(&e, &sel).is_err());
    }

    // ---- rename/copy: matching by <old>, by <new>, and dedup ----

    #[test]
    fn include_a_rename_by_its_old_name_contributes_the_new_name() {
        let e = vec![r("src/old.rs", "src/new.rs")];
        let sel = Selection::Include(patterns(&["src/old.rs"]));
        let result = resolve_scope(&e, &sel).unwrap();
        assert_eq!(result, vec!["src/new.rs"]);
    }

    #[test]
    fn include_a_rename_by_its_new_name_contributes_the_new_name() {
        let e = vec![r("src/old.rs", "src/new.rs")];
        let sel = Selection::Include(patterns(&["src/new.rs"]));
        let result = resolve_scope(&e, &sel).unwrap();
        assert_eq!(result, vec!["src/new.rs"]);
    }

    #[test]
    fn including_both_old_and_new_of_one_rename_still_contributes_new_once() {
        let e = vec![r("src/old.rs", "src/new.rs")];
        let sel = Selection::Include(patterns(&["src/old.rs", "src/new.rs"]));
        let result = resolve_scope(&e, &sel).unwrap();
        assert_eq!(result, vec!["src/new.rs"]);
    }

    #[test]
    fn an_independently_modified_copy_source_is_included_on_its_own_terms() {
        // The copy's C line and the source's own M line are independent; naming the
        // source includes its own M line's contribution (the source itself), not just
        // the copy's <new> mapping.
        let e = vec![c("orig.rs", "copy.rs"), m("orig.rs")];
        let sel = Selection::Include(patterns(&["orig.rs"]));
        let result = resolve_scope(&e, &sel).unwrap();
        assert_eq!(result, vec!["copy.rs", "orig.rs"]);
    }

    #[test]
    fn excluding_the_new_name_of_a_copy_leaves_an_independent_source_edit_untouched() {
        let e = vec![c("orig.rs", "copy.rs"), m("orig.rs")];
        let sel = Selection::Exclude(patterns(&["copy.rs"]));
        let result = resolve_scope(&e, &sel).unwrap();
        assert_eq!(result, vec!["orig.rs"]);
    }

    #[test]
    fn from_cli_prefers_include_and_normalizes_patterns() {
        let sel = Selection::from_cli(&[PathBuf::from("./src/")], &[]);
        assert_eq!(
            sel,
            Selection::Include(vec![pathnorm::normalize_str("src")])
        );
    }

    #[test]
    fn from_cli_falls_back_to_exclude() {
        let sel = Selection::from_cli(&[], &[PathBuf::from("a.rs")]);
        assert_eq!(
            sel,
            Selection::Exclude(vec![pathnorm::normalize_str("a.rs")])
        );
    }

    #[test]
    fn from_cli_is_all_when_neither_given() {
        assert_eq!(Selection::from_cli(&[], &[]), Selection::All);
    }

    #[test]
    fn a_directory_pattern_matches_a_rename_by_its_old_directory() {
        let e = vec![r("old_dir/a.rs", "new_dir/a.rs")];
        let sel = Selection::Include(patterns(&["old_dir"]));
        let result = resolve_scope(&e, &sel).unwrap();
        assert_eq!(result, vec!["new_dir/a.rs"]);
    }
}
