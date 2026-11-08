//! Conversions between the LSP wire types and `nav-core`'s simple model.

use nav_core::{
    file_uri, uri_to_path, OccurrenceRole, Position as NavPosition, Range as NavRange, SymbolKind,
};
use serde_json::Value;
use std::str::FromStr;
use tower_lsp_server::ls_types as lsp;

/// Client URIs are percent-encoded; index keys are produced by `file_uri`.
pub fn canonical(uri: &lsp::Uri) -> String {
    uri.to_file_path()
        .map(|path| file_uri(path.as_ref()))
        .unwrap_or_else(|| uri.as_str().to_string())
}

pub fn canonical_string(uri: &str) -> String {
    lsp::Uri::from_str(uri)
        .map(|parsed| canonical(&parsed))
        .unwrap_or_else(|_| uri.to_string())
}

pub fn to_lsp_uri(uri: &str) -> Option<lsp::Uri> {
    lsp::Uri::from_str(uri)
        .ok()
        .or_else(|| lsp::Uri::from_file_path(uri_to_path(uri)))
}

pub fn to_lsp_range(range: NavRange) -> lsp::Range {
    lsp::Range {
        start: lsp::Position {
            line: range.start.line,
            character: range.start.character,
        },
        end: lsp::Position {
            line: range.end.line,
            character: range.end.character,
        },
    }
}

pub fn to_nav_position(position: lsp::Position) -> NavPosition {
    NavPosition {
        line: position.line,
        character: position.character,
    }
}

pub fn to_lsp_location(uri: &str, range: NavRange) -> Option<lsp::Location> {
    Some(lsp::Location {
        uri: to_lsp_uri(uri)?,
        range: to_lsp_range(range),
    })
}

pub fn line_location(uri: &str, line: u32) -> Option<lsp::Location> {
    let position = lsp::Position { line, character: 0 };
    Some(lsp::Location {
        uri: to_lsp_uri(uri)?,
        range: lsp::Range {
            start: position,
            end: position,
        },
    })
}

pub fn symbol_kind(kind: SymbolKind) -> lsp::SymbolKind {
    match kind {
        SymbolKind::Class => lsp::SymbolKind::CLASS,
        SymbolKind::Interface => lsp::SymbolKind::INTERFACE,
        SymbolKind::Enum => lsp::SymbolKind::ENUM,
        SymbolKind::Annotation => lsp::SymbolKind::INTERFACE,
        SymbolKind::Method | SymbolKind::Synthetic => lsp::SymbolKind::METHOD,
        SymbolKind::Constructor => lsp::SymbolKind::CONSTRUCTOR,
        SymbolKind::Field => lsp::SymbolKind::FIELD,
        SymbolKind::Local | SymbolKind::Parameter => lsp::SymbolKind::VARIABLE,
        SymbolKind::XmlStatement => lsp::SymbolKind::STRING,
    }
}

pub fn is_type_kind(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum | SymbolKind::Annotation
    )
}

pub fn highlight_kind(role: OccurrenceRole) -> lsp::DocumentHighlightKind {
    match role {
        OccurrenceRole::Write => lsp::DocumentHighlightKind::WRITE,
        OccurrenceRole::Read | OccurrenceRole::Reference | OccurrenceRole::Call => {
            lsp::DocumentHighlightKind::READ
        }
        OccurrenceRole::Definition | OccurrenceRole::Type | OccurrenceRole::Import => {
            lsp::DocumentHighlightKind::TEXT
        }
    }
}

fn string_array(values: &[Value]) -> Vec<String> {
    values
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

pub fn parse_scan_config(options: Option<&Value>) -> nav_core::ScanConfig {
    let mut config = nav_core::ScanConfig::default();
    if let Some(options) = options {
        apply_scan_settings(&mut config, options);
    }
    config
}

/// Applies only the keys that are present, so a partial `didChangeConfiguration`
/// cannot wipe the rest of the configuration. Accepts both a flat object and a
/// `{"ld-java-analyze": {...}}` wrapper (what vscode-languageclient sends).
pub fn apply_scan_settings(config: &mut nav_core::ScanConfig, settings: &Value) {
    let settings = settings
        .get("ld-java-analyze")
        .filter(|value| value.is_object())
        .unwrap_or(settings);
    if let Some(values) = settings
        .get("referenceRoots")
        .or_else(|| settings.get("reference_roots"))
        .and_then(Value::as_array)
    {
        config.reference_roots = string_array(values);
    }
    if let Some(values) = settings
        .get("excludeRules")
        .or_else(|| settings.get("exclude_rules"))
        .or_else(|| settings.get("exclude"))
        .and_then(Value::as_array)
    {
        config.exclude_rules = string_array(values);
    }
    if let Some(flag) = settings
        .get("useBuiltinExcludes")
        .or_else(|| settings.get("use_builtin_excludes"))
        .and_then(Value::as_bool)
    {
        config.use_builtin_excludes = flag;
    }
}

/// Identifier under the caret plus an optional receiver to its left.
/// LSP columns count UTF-16 units, so the scan works on UTF-16 offsets.
pub fn word_and_receiver(text: &str, position: NavPosition) -> Option<(String, Option<String>)> {
    let line = text.lines().nth(position.line as usize)?;
    let units: Vec<(char, usize)> = line.chars().map(|c| (c, c.len_utf16())).collect();
    let mut offsets = Vec::with_capacity(units.len());
    let mut total = 0usize;
    for (_, width) in &units {
        offsets.push(total);
        total += width;
    }
    let caret = position.character as usize;
    if caret > total {
        return None;
    }
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut start = offsets
        .iter()
        .rposition(|offset| *offset <= caret)
        .unwrap_or(units.len().saturating_sub(1));
    while start > 0 && is_ident(units[start - 1].0) {
        start -= 1;
    }
    let mut end = start;
    while end < units.len() && is_ident(units[end].0) {
        end += 1;
    }
    if start == end {
        return None;
    }
    let word: String = units[start..end].iter().map(|(c, _)| *c).collect();

    let mut before = start;
    while before > 0 && units[before - 1].0.is_whitespace() {
        before -= 1;
    }
    let receiver = if before > 0 && units[before - 1].0 == '.' {
        let mut left = before - 1;
        while left > 0 && is_ident(units[left - 1].0) {
            left -= 1;
        }
        (left < before - 1).then(|| units[left..before - 1].iter().map(|(c, _)| *c).collect())
    } else {
        None
    };
    Some((word, receiver))
}

/// When the caret sits inside an annotation's argument list, returns the
/// annotation's simple name so an attribute like `summary` can be resolved to
/// `io.swagger...Operation.summary()`.
pub fn annotation_owner(text: &str, position: NavPosition) -> Option<String> {
    let line = text.lines().nth(position.line as usize)?;
    let caret = (position.character as usize).min(line.len());
    let before = &line[..caret];
    let mut search = 0usize;
    let mut found = None;
    while let Some(relative) = before[search..].find('@') {
        let start = search + relative + 1;
        let rest = &before[start..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        if !name.is_empty()
            && name.chars().next().is_some_and(char::is_uppercase)
            && rest[name.len()..].contains('(')
        {
            found = Some(name.rsplit('.').next().unwrap_or(&name).to_string());
        }
        search = start;
    }
    found
}

/// Resolves a simple name to a library FQN using imports and the workspace index.
pub fn import_for(text: &str, simple: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("import ") else {
            continue;
        };
        let fqn = rest.trim().trim_end_matches(';').trim();
        if fqn.rsplit('.').next() == Some(simple) {
            return Some(fqn.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_word_and_receiver() {
        let text = "return service.findById(1);";
        let (word, receiver) = word_and_receiver(
            text,
            NavPosition {
                line: 0,
                character: 20,
            },
        )
        .unwrap();
        assert_eq!(word, "findById");
        assert_eq!(receiver.as_deref(), Some("service"));
    }

    #[test]
    fn annotation_owner_detects_attribute_context() {
        let text = "    @Operation(summary = \"创建用户\")\n    public void create() {}\n";
        let owner = annotation_owner(
            text,
            NavPosition {
                line: 0,
                character: 16,
            },
        );
        assert_eq!(owner.as_deref(), Some("Operation"));
        assert_eq!(
            annotation_owner(
                text,
                NavPosition {
                    line: 1,
                    character: 20
                }
            ),
            None
        );
    }

    #[test]
    fn scan_settings_parse_rules_and_partial_updates() {
        let options = serde_json::json!({
            "referenceRoots": ["src"],
            "excludeRules": ["generated/", "file:*.generated.java"],
            "useBuiltinExcludes": false
        });
        let config = parse_scan_config(Some(&options));
        assert_eq!(config.reference_roots, vec!["src".to_string()]);
        assert_eq!(config.exclude_rules.len(), 2);
        assert!(!config.use_builtin_excludes);

        let mut changed = config.clone();
        apply_scan_settings(
            &mut changed,
            &serde_json::json!({ "ld-java-analyze": { "excludeRules": ["build/"] } }),
        );
        assert_eq!(changed.exclude_rules, vec!["build/".to_string()]);
        assert_eq!(changed.reference_roots, vec!["src".to_string()]);
        assert!(!changed.use_builtin_excludes);
    }

    #[test]
    fn import_lookup_handles_non_import_lines() {
        let text = "package demo;\nimport java.util.List;\nclass A {}\n";
        assert_eq!(import_for(text, "List").as_deref(), Some("java.util.List"));
        assert_eq!(import_for(text, "Missing"), None);
    }
}
