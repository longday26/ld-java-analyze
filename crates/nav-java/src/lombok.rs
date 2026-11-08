//! Minimal Lombok synthetic symbols.
//!
//! Generated getters/setters have no source declaration, so they are injected as
//! `SymbolOrigin::Synthetic` symbols pointing at the backing field. They then take
//! part in definition, references and highlight queries like any ordinary symbol.

use nav_core::{symbol_id, FileContribution, Symbol, SymbolKind, SymbolOrigin};
#[cfg(test)]
use nav_core::{Position, Range};
use std::collections::HashMap;

fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn synthetic(
    owner: &str,
    name: &str,
    descriptor: &str,
    field: &Symbol,
    discriminant: &str,
) -> Symbol {
    Symbol {
        id: symbol_id(&["lombok", owner, name, descriptor, discriminant]),
        kind: SymbolKind::Synthetic,
        name: name.to_string(),
        owner_fqn: Some(owner.to_string()),
        fqn: format!("{owner}.{name}{descriptor}"),
        descriptor: descriptor.to_string(),
        uri: field.uri.clone(),
        range: field.range,
        selection_range: field.selection_range,
        origin: SymbolOrigin::Synthetic,
        synthetic: true,
    }
}

/// `class_annotations` maps an owner FQN to the simple names of its annotations.
pub fn synthesize(
    contrib: &mut FileContribution,
    class_annotations: &HashMap<String, Vec<String>>,
) {
    let fields: Vec<Symbol> = contrib
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Field)
        .cloned()
        .collect();
    let classes: Vec<Symbol> = contrib
        .symbols
        .iter()
        .filter(|symbol| matches!(symbol.kind, SymbolKind::Class | SymbolKind::Enum))
        .cloned()
        .collect();

    for field in &fields {
        let owner = field.owner_fqn.clone().unwrap_or_default();
        let annotations = class_annotations.get(&owner);
        let Some(annotations) = annotations else {
            continue;
        };
        let has = |name: &str| annotations.iter().any(|a| a == name);
        let data = has("Data");
        let cap = capitalized(&field.name);

        if data || has("Getter") {
            let name = if field.descriptor == "boolean" {
                format!("is{cap}")
            } else {
                format!("get{cap}")
            };
            contrib
                .symbols
                .push(synthetic(&owner, &name, "()", field, "getter"));
        }
        if data || has("Setter") {
            let name = format!("set{cap}");
            contrib
                .symbols
                .push(synthetic(&owner, &name, "(1)", field, "setter"));
        }
    }

    for class in &classes {
        let annotations = class_annotations.get(&class.fqn);
        let Some(annotations) = annotations else {
            continue;
        };
        let has = |name: &str| annotations.iter().any(|a| a == name);
        let constructors = [
            ("NoArgsConstructor", "<init>()"),
            ("RequiredArgsConstructor", "<init>(required)"),
            ("AllArgsConstructor", "<init>(all)"),
        ];
        for (annotation, signature) in constructors {
            if has(annotation) || (annotation == "RequiredArgsConstructor" && has("Data")) {
                let mut symbol = synthetic(&class.fqn, signature, "", class, annotation);
                symbol.range = class.range;
                symbol.selection_range = class.selection_range;
                contrib.symbols.push(symbol);
            }
        }
    }
}

/// Placeholder helper used by tests to build a field symbol quickly.
#[cfg(test)]
pub(crate) fn field_symbol(owner: &str, name: &str, descriptor: &str) -> Symbol {
    let position = Position {
        line: 1,
        character: 4,
    };
    Symbol {
        id: symbol_id(&["test", owner, name]),
        kind: SymbolKind::Field,
        name: name.to_string(),
        owner_fqn: Some(owner.to_string()),
        fqn: format!("{owner}.{name}"),
        descriptor: descriptor.to_string(),
        uri: "file:///Sample.java".to_string(),
        range: Range::new(position, position),
        selection_range: Range::new(position, position),
        origin: SymbolOrigin::Workspace,
        synthetic: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_generates_getter_and_setter_pointing_at_field() {
        let field = field_symbol("demo.User", "name", "String");
        let mut contrib = FileContribution {
            uri: "file:///Sample.java".into(),
            symbols: vec![field.clone()],
            ..Default::default()
        };
        let mut annotations = HashMap::new();
        annotations.insert("demo.User".to_string(), vec!["Data".to_string()]);

        synthesize(&mut contrib, &annotations);

        let names: Vec<&str> = contrib.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"getName"));
        assert!(names.contains(&"setName"));
        assert!(contrib
            .symbols
            .iter()
            .all(|s| s.synthetic || s.id == field.id));
        let getter = contrib
            .symbols
            .iter()
            .find(|s| s.name == "getName")
            .unwrap();
        assert_eq!(getter.selection_range, field.selection_range);
    }

    #[test]
    fn boolean_field_uses_is_prefix() {
        let field = field_symbol("demo.User", "active", "boolean");
        let mut contrib = FileContribution {
            symbols: vec![field],
            ..Default::default()
        };
        let mut annotations = HashMap::new();
        annotations.insert("demo.User".to_string(), vec!["Getter".to_string()]);

        synthesize(&mut contrib, &annotations);

        assert!(contrib.symbols.iter().any(|s| s.name == "isActive"));
        assert!(!contrib.symbols.iter().any(|s| s.name == "setActive"));
    }
}
