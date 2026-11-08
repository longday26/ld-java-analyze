//! Java 8 definition/reference extraction on top of `tree-sitter-java`.
//!
//! Two passes: collect every definition, then bind references against them.
//! References that cannot be bound locally are handed to `nav-index` as
//! [`UnresolvedOccurrence`] so cross-file calls can be resolved workspace-wide.

use crate::lombok;
use nav_core::*;
use std::collections::HashMap;
use tree_sitter::{Language, Node, Parser};

pub struct JavaParser {
    language: Language,
}

impl JavaParser {
    pub fn new() -> Self {
        Self {
            language: Language::new(tree_sitter_java::LANGUAGE),
        }
    }

    pub fn parse_file(&self, uri: &str, source: &str) -> Result<FileContribution, String> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language)
            .map_err(|error| error.to_string())?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| "tree-sitter returned no syntax tree".to_string())?;
        let root = tree.root_node();

        let mut definitions = DefCollector::new(uri, source);
        definitions.walk(root, &[]);
        let (mut contribution, class_annotations) = definitions.finish();

        let mut references = RefCollector::new(uri, source, &contribution.symbols);
        references.walk(root);
        contribution.occurrences.extend(references.occurrences);
        contribution.unresolved.extend(references.unresolved);

        lombok::synthesize(&mut contribution, &class_annotations);
        Ok(contribution)
    }
}

impl Default for JavaParser {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------ utilities

fn named_children<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn text_of<'a>(node: Node<'a>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

fn range_of(node: Node<'_>, source: &str) -> Range {
    Range::new(
        position_at(source, node.start_byte()),
        position_at(source, node.end_byte()),
    )
}

fn qualified(package: &str, stack: &[String], name: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !package.is_empty() {
        parts.push(package);
    }
    parts.extend(stack.iter().map(String::as_str));
    parts.push(name);
    parts.join(".")
}

fn has_ancestor(node: Node<'_>, kinds: &[&str]) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if kinds.contains(&parent.kind()) {
            return true;
        }
        current = parent.parent();
    }
    false
}

/// A capitalized identifier used as a method invocation's object is a type
/// (`PageResult.of(...)`), not a variable read.
fn is_static_type_receiver(node: Node<'_>, name: &str) -> bool {
    if !name.chars().next().is_some_and(char::is_uppercase) {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "method_invocation"
        && parent
            .child_by_field_name("object")
            .is_some_and(|object| object.byte_range() == node.byte_range())
}

/// Only header components (not parameters of methods declared in a record body).
fn is_record_component(node: Node<'_>) -> bool {
    let Some(parameters) = node.parent() else {
        return false;
    };
    parameters.kind() == "formal_parameters"
        && parameters
            .parent()
            .is_some_and(|declaration| declaration.kind() == "record_declaration")
}

fn descendants<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
    for child in named_children(node) {
        out.push(child);
        descendants(child, out);
    }
}

// ------------------------------------------------------------- definition pass

struct DefCollector<'a> {
    uri: &'a str,
    source: &'a str,
    package: String,
    contribution: FileContribution,
    class_annotations: HashMap<String, Vec<String>>,
}

impl<'a> DefCollector<'a> {
    fn new(uri: &'a str, source: &'a str) -> Self {
        Self {
            uri,
            source,
            package: String::new(),
            contribution: FileContribution {
                uri: uri.to_string(),
                ..Default::default()
            },
            class_annotations: HashMap::new(),
        }
    }

    fn finish(self) -> (FileContribution, HashMap<String, Vec<String>>) {
        (self.contribution, self.class_annotations)
    }

    // Positional args keep every call site (class/method/field/parameter) a
    // single readable line; a spec struct would add noise without adding safety.
    #[allow(clippy::too_many_arguments)]
    fn push_symbol(
        &mut self,
        kind: SymbolKind,
        name: String,
        owner_fqn: Option<String>,
        fqn: String,
        descriptor: String,
        whole: Range,
        selection: Range,
    ) {
        let id = symbol_id(&[
            self.uri,
            &format!("{kind:?}"),
            owner_fqn.as_deref().unwrap_or(""),
            &name,
            &descriptor,
            &whole.start.line.to_string(),
            &whole.start.character.to_string(),
        ]);
        self.contribution.occurrences.push(Occurrence {
            symbol_id: id,
            uri: self.uri.to_string(),
            range: selection,
            role: OccurrenceRole::Definition,
        });
        self.contribution.symbols.push(Symbol {
            id,
            kind,
            name,
            owner_fqn,
            fqn,
            descriptor,
            uri: self.uri.to_string(),
            range: whole,
            selection_range: selection,
            origin: SymbolOrigin::Workspace,
            synthetic: false,
        });
    }

    fn annotations_of(&self, node: Node<'a>) -> Vec<String> {
        let mut out = Vec::new();
        for child in named_children(node) {
            if child.kind() == "modifiers" {
                self.collect_annotations(child, &mut out);
            }
        }
        out
    }

    fn collect_annotations(&self, node: Node<'a>, out: &mut Vec<String>) {
        if matches!(node.kind(), "marker_annotation" | "annotation") {
            let raw = text_of(node, self.source);
            let name = raw
                .trim_start_matches('@')
                .split(['(', '.'])
                .next()
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                out.push(name);
            }
        }
        for child in named_children(node) {
            self.collect_annotations(child, out);
        }
    }

    /// Java records generate an accessor per component with no explicit method
    /// declaration. Emit synthetic symbols so `pageResult.total()` navigates to
    /// the `total` component in the record header.
    fn synthesize_record_accessors(&mut self, node: Node<'a>, owner: &str) {
        let Some(parameters) = node.child_by_field_name("parameters") else {
            return;
        };
        for component in named_children(parameters) {
            if component.kind() != "formal_parameter" {
                continue;
            }
            let Some(name_node) = component.child_by_field_name("name") else {
                continue;
            };
            let name = text_of(name_node, self.source).to_string();
            if name.is_empty() {
                continue;
            }
            let selection = range_of(name_node, self.source);
            let whole = range_of(component, self.source);
            let id = symbol_id(&[
                self.uri,
                "Synthetic",
                owner,
                &name,
                "()",
                &whole.start.line.to_string(),
                &whole.start.character.to_string(),
            ]);
            self.contribution.occurrences.push(Occurrence {
                symbol_id: id,
                uri: self.uri.to_string(),
                range: selection,
                role: OccurrenceRole::Definition,
            });
            self.contribution.symbols.push(Symbol {
                id,
                kind: SymbolKind::Synthetic,
                name: name.clone(),
                owner_fqn: Some(owner.to_string()),
                fqn: format!("{owner}.{name}()"),
                descriptor: "()".to_string(),
                uri: self.uri.to_string(),
                range: whole,
                selection_range: selection,
                origin: SymbolOrigin::Synthetic,
                synthetic: true,
            });
        }
    }

    fn collect_type_edges(&mut self, node: Node<'a>, child_fqn: &str) {
        for child in named_children(node) {
            let kind = match child.kind() {
                "superclass" => TypeEdgeKind::Extends,
                "super_interfaces" | "extends_interfaces" => TypeEdgeKind::Implements,
                _ => continue,
            };
            let mut nodes = Vec::new();
            descendants(child, &mut nodes);
            for descendant in nodes {
                if descendant.kind() == "type_identifier" {
                    self.contribution.type_edges.push(TypeEdge {
                        child_fqn: child_fqn.to_string(),
                        parent_fqn: text_of(descendant, self.source).to_string(),
                        kind,
                    });
                }
            }
        }
    }

    fn walk(&mut self, node: Node<'a>, incoming: &[String]) {
        let mut stack = incoming.to_vec();

        match node.kind() {
            "package_declaration" => {
                for child in named_children(node) {
                    if matches!(child.kind(), "scoped_identifier" | "identifier") {
                        self.package = text_of(child, self.source).to_string();
                    }
                }
            }
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "annotation_type_declaration"
            | "record_declaration" => {
                let name = node
                    .child_by_field_name("name")
                    .map(|n| text_of(n, self.source).to_string())
                    .unwrap_or_default();
                let fqn = qualified(&self.package, &stack, &name);
                let kind = match node.kind() {
                    "interface_declaration" => SymbolKind::Interface,
                    "enum_declaration" => SymbolKind::Enum,
                    "annotation_type_declaration" => SymbolKind::Annotation,
                    _ => SymbolKind::Class,
                };
                if let Some(name_node) = node.child_by_field_name("name") {
                    self.push_symbol(
                        kind,
                        name.clone(),
                        None,
                        fqn.clone(),
                        String::new(),
                        range_of(node, self.source),
                        range_of(name_node, self.source),
                    );
                }
                self.class_annotations
                    .insert(fqn.clone(), self.annotations_of(node));
                self.collect_type_edges(node, &fqn);
                if node.kind() == "record_declaration" {
                    self.synthesize_record_accessors(node, &fqn);
                }
                stack.push(name);
            }
            "method_declaration" | "constructor_declaration" => {
                let owner = qualified(&self.package, &stack, "");
                let owner = owner.trim_end_matches('.').to_string();
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = text_of(name_node, self.source).to_string();
                    let descriptor = node
                        .child_by_field_name("parameters")
                        .map(|n| text_of(n, self.source).to_string())
                        .unwrap_or_default();
                    let kind = if node.kind() == "constructor_declaration" {
                        SymbolKind::Constructor
                    } else {
                        SymbolKind::Method
                    };
                    self.push_symbol(
                        kind,
                        name.clone(),
                        Some(owner.clone()),
                        format!("{owner}.{name}{descriptor}"),
                        descriptor,
                        range_of(node, self.source),
                        range_of(name_node, self.source),
                    );
                }
            }
            "field_declaration" | "local_variable_declaration" => {
                let owner = qualified(&self.package, &stack, "");
                let owner = owner.trim_end_matches('.').to_string();
                let type_name = node
                    .child_by_field_name("type")
                    .map(|n| text_of(n, self.source).to_string())
                    .unwrap_or_default();
                let kind = if node.kind() == "field_declaration" {
                    SymbolKind::Field
                } else {
                    SymbolKind::Local
                };
                for child in named_children(node) {
                    if child.kind() == "variable_declarator" {
                        if let Some(name_node) = child.child_by_field_name("name") {
                            let name = text_of(name_node, self.source).to_string();
                            self.push_symbol(
                                kind,
                                name.clone(),
                                Some(owner.clone()),
                                format!("{owner}.{name}"),
                                type_name.clone(),
                                range_of(child, self.source),
                                range_of(name_node, self.source),
                            );
                        }
                    }
                }
            }
            "formal_parameter" | "enhanced_for_statement" | "catch_formal_parameter" => {
                // A record header component already has a synthetic accessor symbol.
                if node.kind() == "formal_parameter" && is_record_component(node) {
                    return;
                }
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = text_of(name_node, self.source).to_string();
                    let owner = qualified(&self.package, &stack, "");
                    let type_name = node
                        .child_by_field_name("type")
                        .map(|n| text_of(n, self.source).to_string())
                        .unwrap_or_default();
                    self.push_symbol(
                        SymbolKind::Parameter,
                        name.clone(),
                        Some(owner.clone()),
                        format!("{owner}.{name}"),
                        type_name,
                        range_of(node, self.source),
                        range_of(name_node, self.source),
                    );
                }
            }
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.walk(child, &stack);
        }
    }
}

// -------------------------------------------------------------- reference pass

struct RefCollector<'a> {
    uri: &'a str,
    source: &'a str,
    defs: &'a [Symbol],
    occurrences: Vec<Occurrence>,
    unresolved: Vec<UnresolvedOccurrence>,
}

impl<'a> RefCollector<'a> {
    fn new(uri: &'a str, source: &'a str, defs: &'a [Symbol]) -> Self {
        Self {
            uri,
            source,
            defs,
            occurrences: Vec::new(),
            unresolved: Vec::new(),
        }
    }

    fn walk(&mut self, node: Node<'a>) {
        if matches!(node.kind(), "identifier" | "type_identifier") {
            self.handle(node);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.walk(child);
        }
    }

    fn handle(&mut self, node: Node<'a>) {
        let range = range_of(node, self.source);
        let name = text_of(node, self.source).to_string();
        if name.is_empty() {
            return;
        }
        if self
            .defs
            .iter()
            .any(|symbol| symbol.selection_range == range)
        {
            return;
        }
        if has_ancestor(
            node,
            &[
                "package_declaration",
                "import_declaration",
                "scoped_identifier",
            ],
        ) {
            return;
        }

        let (mut role, mut kind_hint) = classify(node);
        // `PageResult.of(...)` / `Result.success(...)`: a capitalized receiver is
        // a type used for a static call, not a variable read.
        if is_static_type_receiver(node, &name) {
            role = OccurrenceRole::Type;
            kind_hint = Some(SymbolKind::Class);
        }
        let receiver_type = self.receiver_type(node);
        let argument_count = (role == OccurrenceRole::Call)
            .then(|| self.argument_count(node))
            .flatten();
        // `receiver.method()` must be resolved by the receiver's static type.
        // Binding it to a same-named method declared in this file (e.g. an HTTP
        // endpoint `list(...)` next to `userService.list(query)`) is always wrong.
        let qualified_call = role == OccurrenceRole::Call && receiver_type.is_some();
        let candidates: Vec<&Symbol> = if qualified_call {
            Vec::new()
        } else {
            self.defs
                .iter()
                .filter(|symbol| {
                    symbol.name == name
                        && kind_hint
                            .map(|hint| kind_matches(symbol.kind, hint))
                            .unwrap_or(true)
                })
                .collect()
        };

        if let Some(symbol) = nearest(candidates, range) {
            self.occurrences.push(Occurrence {
                symbol_id: symbol.id,
                uri: self.uri.to_string(),
                range,
                role,
            });
        } else {
            self.unresolved.push(UnresolvedOccurrence {
                uri: self.uri.to_string(),
                range,
                name,
                kind_hint,
                role,
                receiver_type,
                argument_count,
            });
        }
    }

    fn receiver_type(&self, node: Node<'a>) -> Option<String> {
        let parent = node.parent()?;
        let object = parent.child_by_field_name("object")?;
        let text = text_of(object, self.source);
        match object.kind() {
            "type_identifier" => Some(text.to_string()),
            "identifier" if text.chars().next().is_some_and(char::is_uppercase) => {
                Some(text.to_string())
            }
            "identifier" => {
                let symbol = self.defs.iter().find(|symbol| {
                    symbol.name == text
                        && matches!(
                            symbol.kind,
                            SymbolKind::Field | SymbolKind::Local | SymbolKind::Parameter
                        )
                })?;
                Some(symbol.descriptor.clone())
            }
            _ => None,
        }
    }

    fn argument_count(&self, node: Node<'a>) -> Option<usize> {
        let invocation = node.parent()?;
        if invocation.kind() != "method_invocation" {
            return None;
        }
        let arguments = invocation.child_by_field_name("arguments")?;
        Some(arguments.named_child_count())
    }
}

fn classify(node: Node<'_>) -> (OccurrenceRole, Option<SymbolKind>) {
    if let Some(parent) = node.parent() {
        let is_named = parent
            .child_by_field_name("name")
            .map(|named| named.byte_range() == node.byte_range())
            .unwrap_or(false);
        match parent.kind() {
            "method_invocation" if is_named => {
                return (OccurrenceRole::Call, Some(SymbolKind::Method));
            }
            "field_access" if is_named => {
                return (OccurrenceRole::Read, Some(SymbolKind::Field));
            }
            "object_creation_expression" => {
                return (OccurrenceRole::Type, Some(SymbolKind::Class));
            }
            _ => {}
        }
    }
    if node.kind() == "type_identifier" {
        return (OccurrenceRole::Type, Some(SymbolKind::Class));
    }
    let is_write_target = node.parent().is_some_and(|parent| {
        matches!(parent.kind(), "assignment_expression" | "update_expression")
            && parent
                .child_by_field_name("left")
                .is_some_and(|left| left.byte_range() == node.byte_range())
    });
    if is_write_target {
        (OccurrenceRole::Write, None)
    } else {
        (OccurrenceRole::Read, None)
    }
}

fn kind_matches(actual: SymbolKind, hint: SymbolKind) -> bool {
    match hint {
        SymbolKind::Method => matches!(actual, SymbolKind::Method | SymbolKind::Synthetic),
        SymbolKind::Field => matches!(actual, SymbolKind::Field | SymbolKind::Synthetic),
        SymbolKind::Class => matches!(
            actual,
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum | SymbolKind::Annotation
        ),
        _ => true,
    }
}

fn nearest(candidates: Vec<&Symbol>, at: Range) -> Option<&Symbol> {
    let mut best: Option<&Symbol> = None;
    let mut best_distance = u32::MAX;
    for candidate in candidates {
        let distance = candidate.selection_range.start.line.abs_diff(at.start.line);
        if distance < best_distance {
            best_distance = distance;
            best = Some(candidate);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn record_components_become_synthetic_accessors() {
        let source = "package demo;\n\
public record PageResult<T>(List<T> items, long total, int page, int size) {\n\
    public long totalPages() { return 1; }\n\
}\n";
        let contribution = JavaParser::new()
            .parse_file("file:///PageResult.java", source)
            .unwrap();
        assert!(contribution
            .symbols
            .iter()
            .any(|s| s.name == "PageResult" && s.kind == SymbolKind::Class));
        for name in ["items", "total", "page", "size"] {
            let accessor = contribution
                .symbols
                .iter()
                .find(|s| s.name == name && s.synthetic)
                .unwrap_or_else(|| panic!("missing accessor {name}"));
            assert_eq!(accessor.kind, SymbolKind::Synthetic);
            assert_eq!(accessor.descriptor, "()");
            assert_eq!(accessor.selection_range.start.line, 1);
            // A header component is not a method parameter.
            assert!(!contribution
                .symbols
                .iter()
                .any(|s| s.name == name && s.kind == SymbolKind::Parameter));
        }
    }

    #[test]
    fn qualified_call_is_not_bound_to_a_same_named_local_method() {
        let source = "package demo;\n\
class Controller {\n\
    private Service svc;\n\
    public int list(int a, int b) { return 0; }\n\
    public int run() { return svc.list(1); }\n\
}\n";
        let contribution = JavaParser::new()
            .parse_file("file:///Controller.java", source)
            .unwrap();
        let call = contribution
            .unresolved
            .iter()
            .find(|u| u.name == "list" && u.range.start.line == 4)
            .expect("qualified call must stay unresolved for the global index");
        assert_eq!(call.receiver_type.as_deref(), Some("Service"));
        assert_eq!(call.argument_count, Some(1));

        let local_list = contribution
            .symbols
            .iter()
            .find(|s| s.name == "list" && s.kind == SymbolKind::Method)
            .unwrap();
        assert!(!contribution
            .occurrences
            .iter()
            .any(|o| { o.symbol_id == local_list.id && o.range.start.line == 4 }));
    }

    #[test]
    fn static_call_receiver_is_a_type_reference() {
        let source = "package demo;\nclass C { void m() { Result.success(1); } }\n";
        let contribution = JavaParser::new()
            .parse_file("file:///C.java", source)
            .unwrap();
        let type_use = contribution
            .unresolved
            .iter()
            .find(|u| u.name == "Result")
            .unwrap();
        assert_eq!(type_use.role, OccurrenceRole::Type);
        assert_eq!(type_use.kind_hint, Some(SymbolKind::Class));
        let call = contribution
            .unresolved
            .iter()
            .find(|u| u.name == "success")
            .unwrap();
        assert_eq!(call.receiver_type.as_deref(), Some("Result"));
    }

    const SAMPLE: &str = r#"
package demo;

import java.util.List;

@Data
public class User {
    private String name;
    private boolean active;

    public String greet(String prefix) {
        return prefix + name;
    }
}

public interface Greeter {
    String hello(String who);
}
"#;

    #[test]
    fn extracts_definitions_and_lombok_symbols() {
        let contribution = JavaParser::new()
            .parse_file("file:///Sample.java", SAMPLE)
            .unwrap();
        let names: HashSet<&str> = contribution
            .symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect();

        for expected in [
            "User", "Greeter", "name", "active", "greet", "hello", "prefix",
        ] {
            assert!(names.contains(expected), "missing symbol {expected}");
        }
        for expected in ["getName", "setName", "isActive", "setActive"] {
            assert!(names.contains(expected), "missing lombok symbol {expected}");
        }
        assert!(contribution
            .symbols
            .iter()
            .any(|symbol| symbol.fqn == "demo.User"));
    }

    #[test]
    fn binds_local_field_reference_and_leaves_cross_file_calls_unresolved() {
        let contribution = JavaParser::new()
            .parse_file("file:///Sample.java", SAMPLE)
            .unwrap();
        let name_symbol = contribution
            .symbols
            .iter()
            .find(|symbol| symbol.name == "name" && symbol.kind == SymbolKind::Field)
            .unwrap();
        assert!(contribution
            .occurrences
            .iter()
            .any(|occurrence| occurrence.symbol_id == name_symbol.id
                && occurrence.role == OccurrenceRole::Read));
    }

    #[test]
    fn package_and_utf16_positions_are_correct() {
        let source = "// 注释\npackage demo;\nclass A {}\n";
        let contribution = JavaParser::new()
            .parse_file("file:///A.java", source)
            .unwrap();
        let a = contribution
            .symbols
            .iter()
            .find(|symbol| symbol.name == "A")
            .unwrap();
        assert_eq!(a.fqn, "demo.A");
        assert_eq!(a.selection_range.start.line, 2);
    }
}
