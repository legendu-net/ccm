//! Human-readable rendering of `api.yaml` entries, shared by `--list-tools` and
//! default mode's tool picker (stage 5, when the tool choice is ambiguous) so the two
//! views can never drift apart (see `pipeline.rs`).

use super::model::{Entry, EntryKind};

/// The `type:` label shown for an entry, matching the value that appears in `api.yaml`
/// itself.
fn type_label(kind: &EntryKind) -> &'static str {
    match kind {
        EntryKind::OpenaiApi(_) => "openai_api",
        EntryKind::AgentCli(_) => "agent_cli",
    }
}

/// The index of the entry [`lines`] marks `(default)` — the same first-`enabled: true`
/// entry `config::validate::select_first_enabled` would pick automatically (whether
/// silently, or via `--dry-run`). `None` if nothing is enabled. Shared with the tool
/// picker's own default (see `pipeline.rs`), so the marker printed here and the entry a
/// blank line at the prompt selects can never drift apart.
#[must_use]
pub fn default_index(entries: &[Entry]) -> Option<usize> {
    entries.iter().position(|e| e.enabled)
}

/// One display line per entry, in `api.yaml` order: `name`, `type`, an
/// `[enabled]`/`[disabled]` marker, and `(default)` on the first enabled entry — the one
/// `select_first_enabled` would pick automatically, absent `--tool`. Names are
/// left-padded to the widest name so the columns line up.
#[must_use]
pub fn lines(entries: &[Entry]) -> Vec<String> {
    let name_width = entries
        .iter()
        .map(|e| e.name.chars().count())
        .max()
        .unwrap_or(0);
    let default_index = default_index(entries);

    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let marker = if entry.enabled {
                "[enabled]"
            } else {
                "[disabled]"
            };
            let mut line = format!(
                "{name:<name_width$}  {kind:<10}  {marker}",
                name = entry.name,
                kind = type_label(&entry.kind),
            );
            if default_index == Some(index) {
                line.push_str("  (default)");
            }
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{ApiKeySource, OpenAiApiEntry};
    use std::collections::HashMap;

    fn openai(name: &str, enabled: bool) -> Entry {
        Entry {
            name: name.to_string(),
            enabled,
            prompt: "default".to_string(),
            kind: EntryKind::OpenaiApi(OpenAiApiEntry {
                base_url: "http://localhost/v1".to_string(),
                model: "m".to_string(),
                api_key: ApiKeySource::Env("K".to_string()),
                max_tokens: None,
                temperature: None,
                headers: HashMap::new(),
                timeout_secs: 60,
            }),
        }
    }

    #[test]
    fn lists_entries_in_order_with_type_and_marker() {
        let entries = vec![openai("a", true), openai("b", false)];
        let lines = lines(&entries);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("a"));
        assert!(lines[0].contains("openai_api"));
        assert!(lines[0].contains("[enabled]"));
        assert!(lines[1].starts_with("b"));
        assert!(lines[1].contains("[disabled]"));
    }

    #[test]
    fn marks_only_the_first_enabled_entry_as_default() {
        let entries = vec![openai("a", false), openai("b", true), openai("c", true)];
        let lines = lines(&entries);
        assert!(!lines[0].contains("(default)"));
        assert!(lines[1].contains("(default)"));
        assert!(!lines[2].contains("(default)"));
    }

    #[test]
    fn no_default_marker_when_nothing_is_enabled() {
        let entries = vec![openai("a", false)];
        let lines = lines(&entries);
        assert!(!lines[0].contains("(default)"));
    }

    #[test]
    fn default_index_matches_the_first_enabled_entry() {
        let entries = vec![openai("a", false), openai("b", true), openai("c", true)];
        assert_eq!(default_index(&entries), Some(1));
    }

    #[test]
    fn default_index_is_none_when_nothing_is_enabled() {
        let entries = vec![openai("a", false)];
        assert_eq!(default_index(&entries), None);
    }

    #[test]
    fn aligns_names_of_uneven_length() {
        let entries = vec![openai("short", true), openai("a-much-longer-name", false)];
        let lines = lines(&entries);
        // Both type columns should start at the same offset.
        let short_type_pos = lines[0].find("openai_api").unwrap();
        let long_type_pos = lines[1].find("openai_api").unwrap();
        assert_eq!(short_type_pos, long_type_pos);
    }
}
