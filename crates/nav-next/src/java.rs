//! Clean-room Java analyzer for `nav-next`.
//!
//! This module does not reuse `nav-java` or `nav-core`. It parses Java source
//! with `tree-sitter-java` and emits the next-generation index contracts.

use crate::model::{
    FileContribution, FileId, Occurrence, OccurrenceRole, Position, Span, Symbol, SymbolId,
    SymbolKind, TypeEdgeKind, UnresolvedOccurrence, UnresolvedTypeEdge,
};
use std::collections::{HashMap, HashSet};
use tree_sitter::{Language, Node, Parser};

pub struct JavaAnalyzer {
    language: Language,
}

impl JavaAnalyzer {
    pub fn new() -> Self {
        Self {
            language: Language::new(tree_sitter_java::LANGUAGE),
        }
    }

    pub fn analyze(&self, file: FileId, source: &str) -> Result<FileContribution, String> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language)
            .map_err(|error| error.to_string())?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| "tree-sitter returned no syntax tree".to_string())?;
        let root = tree.root_node();

        let line_index = LineIndex::new(source);
        let package = package_name(root, source);
        let mut collector = DeclarationWalker {
            file,
            source,
            line_index: &line_index,
            package,
            type_stack: Vec::new(),
            symbols: Vec::new(),
            fqn_to_id: HashMap::new(),
            name_to_ids: HashMap::new(),
            definition_spans: HashSet::new(),
            unresolved_type_edges: Vec::new(),
            owner_fixups: Vec::new(),
        };

        collector.walk(root);
        let (mut contribution, definition_spans) = collector.finish();

        let mut type_environment = TypeEnvironmentWalker {
            source,
            variable_types: HashMap::new(),
        };
        type_environment.walk(root);

        let reference_walker = ReferenceWalker {
            file,
            source,
            line_index: &line_index,
            name_to_ids: collector_local_names(&contribution),
            variable_types: type_environment.variable_types,
            definition_spans,
        };
        reference_walker.walk(root, &mut contribution);

        Ok(contribution)
    }
}

impl Default for JavaAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

// --------------------------------------------------------------------- helpers

fn collector_local_names(contribution: &FileContribution) -> HashMap<String, Vec<SymbolId>> {
    let mut names: HashMap<String, Vec<SymbolId>> = HashMap::new();
    for symbol in &contribution.symbols {
        names
            .entry(symbol.name.clone())
            .or_default()
            .push(symbol.id);
    }
    names
}

fn symbol_id(file: FileId, fqn: &str, descriptor: &str) -> SymbolId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&file.0.to_le_bytes());
    hasher.update(fqn.as_bytes());
    hasher.update(descriptor.as_bytes());
    let hash = hasher.finalize();
    let bytes = hash.as_bytes();
    SymbolId(u64::from_le_bytes(bytes[..8].try_into().unwrap()))
}

fn named_children<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn descendants<'a>(node: Node<'a>, output: &mut Vec<Node<'a>>) {
    for child in named_children(node) {
        output.push(child);
        descendants(child, output);
    }
}

fn text_of<'a>(node: Node<'a>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

fn simple_type_name(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

fn method_owner_simple_name(symbol: &Symbol) -> Option<&str> {
    symbol
        .fqn
        .rsplit_once('.')
        .and_then(|(owner, _)| owner.rsplit_once('.').map(|(_, simple)| simple))
}

fn package_name(root: Node<'_>, source: &str) -> String {
    for child in named_children(root) {
        if child.kind() == "package_declaration" {
            for descendant in named_children(child) {
                if matches!(descendant.kind(), "scoped_identifier" | "identifier") {
                    return text_of(descendant, source).to_string();
                }
            }
        }
    }
    String::new()
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

// ------------------------------------------------------------------ line index

struct LineIndex {
    line_starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let mut line_starts = vec![0];
        for (offset, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self { line_starts }
    }

    fn position(&self, source: &str, byte: usize) -> Position {
        let byte = byte.min(source.len());
        let line = match self.line_starts.binary_search(&byte) {
            Ok(index) => index as u32,
            Err(index) => index.saturating_sub(1) as u32,
        };
        let line_start = self.line_starts[line as usize];
        let character = source[line_start..byte].encode_utf16().count() as u32;
        Position { line, character }
    }

    fn span(&self, source: &str, node: Node<'_>) -> Span {
        Span {
            start: self.position(source, node.start_byte()),
            end: self.position(source, node.end_byte()),
        }
    }
}

// -------------------------------------------------------------- declarations

struct DeclarationWalker<'a> {
    file: FileId,
    source: &'a str,
    line_index: &'a LineIndex,
    package: String,
    type_stack: Vec<String>,
    symbols: Vec<Symbol>,
    fqn_to_id: HashMap<String, SymbolId>,
    name_to_ids: HashMap<String, Vec<SymbolId>>,
    definition_spans: HashSet<(usize, usize)>,
    unresolved_type_edges: Vec<UnresolvedTypeEdge>,
    owner_fixups: Vec<(usize, String)>,
}

impl<'a> DeclarationWalker<'a> {
    fn walk(&mut self, node: Node<'a>) {
        match node.kind() {
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration" => {
                self.visit_type_declaration(node);
            }
            "method_declaration" => self.visit_method_declaration(node),
            "constructor_declaration" => self.visit_constructor_declaration(node),
            "field_declaration" => self.visit_field_declaration(node),
            "enum_constant" => self.visit_enum_constant(node),
            _ => {
                for child in named_children(node) {
                    self.walk(child);
                }
            }
        }
    }

    fn visit_type_declaration(&mut self, node: Node<'a>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = text_of(name_node, self.source).to_string();
        let kind = match node.kind() {
            "interface_declaration" => SymbolKind::Interface,
            "enum_declaration" => SymbolKind::Enum,
            "annotation_type_declaration" => SymbolKind::Annotation,
            "record_declaration" => SymbolKind::Record,
            _ => SymbolKind::Class,
        };
        let fqn = qualified(&self.package, &self.type_stack, &name);
        let id = symbol_id(self.file, &fqn, "");
        let symbol = Symbol {
            id,
            file_id: self.file,
            kind,
            name: name.clone(),
            fqn: fqn.clone(),
            owner: None,
            descriptor: String::new(),
            span: self.line_index.span(self.source, node),
            selection_span: self.line_index.span(self.source, name_node),
            synthetic: false,
        };
        self.definition_spans
            .insert((name_node.start_byte(), name_node.end_byte()));
        self.symbols.push(symbol);
        self.fqn_to_id.insert(fqn.clone(), id);
        self.name_to_ids.entry(name.clone()).or_default().push(id);
        self.collect_type_edges(node, id);

        self.type_stack.push(name);
        for child in named_children(node) {
            if matches!(
                child.kind(),
                "class_body" | "interface_body" | "enum_body" | "record_body" | "constructor_body"
            ) || matches!(
                child.kind(),
                "class_declaration"
                    | "interface_declaration"
                    | "enum_declaration"
                    | "record_declaration"
            ) {
                self.walk(child);
            }
        }
        self.type_stack.pop();
    }

    fn collect_type_edges(&mut self, node: Node<'a>, child_id: SymbolId) {
        let mut parent_nodes = Vec::new();
        if let Some(superclass) = node.child_by_field_name("superclass") {
            parent_nodes.push(superclass);
        }
        if let Some(interfaces) = node.child_by_field_name("interfaces") {
            parent_nodes.push(interfaces);
        }
        for child in named_children(node) {
            if matches!(child.kind(), "extends_interfaces" | "super_interfaces") {
                parent_nodes.push(child);
            }
        }

        for parent_node in parent_nodes {
            let mut descendant_nodes = Vec::new();
            descendants(parent_node, &mut descendant_nodes);
            for descendant in descendant_nodes {
                if descendant.kind() == "type_identifier" {
                    let parent_name = text_of(descendant, self.source).to_string();
                    self.unresolved_type_edges.push(UnresolvedTypeEdge {
                        child: child_id,
                        parent_name,
                        kind: TypeEdgeKind::Implements,
                    });
                }
            }
        }
    }

    fn visit_method_declaration(&mut self, node: Node<'a>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let parameters = node
            .child_by_field_name("parameters")
            .map(|parameters| text_of(parameters, self.source).to_string())
            .unwrap_or_default();
        let descriptor = parameters;
        let name = text_of(name_node, self.source).to_string();
        let owner_fqn = self.current_type_fqn();
        let fqn = match &owner_fqn {
            Some(owner) => format!("{owner}.{name}"),
            None => name.clone(),
        };
        let id = symbol_id(self.file, &fqn, &descriptor);
        let symbol = Symbol {
            id,
            file_id: self.file,
            kind: SymbolKind::Method,
            name: name.clone(),
            fqn: fqn.clone(),
            owner: None,
            descriptor,
            span: self.line_index.span(self.source, node),
            selection_span: self.line_index.span(self.source, name_node),
            synthetic: false,
        };
        self.definition_spans
            .insert((name_node.start_byte(), name_node.end_byte()));
        self.symbols.push(symbol);
        self.name_to_ids.entry(name).or_default().push(id);
        if let Some(owner) = owner_fqn {
            self.owner_fixups.push((self.symbols.len() - 1, owner));
        }
        self.collect_local_symbols(node, &fqn, id);
    }

    fn visit_constructor_declaration(&mut self, node: Node<'a>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let parameters = node
            .child_by_field_name("parameters")
            .map(|parameters| text_of(parameters, self.source).to_string())
            .unwrap_or_default();
        let descriptor = parameters;
        let name = text_of(name_node, self.source).to_string();
        let owner_fqn = self.current_type_fqn();
        let fqn = match &owner_fqn {
            Some(owner) => format!("{owner}.{name}"),
            None => name.clone(),
        };
        let id = symbol_id(self.file, &fqn, &descriptor);
        let symbol = Symbol {
            id,
            file_id: self.file,
            kind: SymbolKind::Constructor,
            name: name.clone(),
            fqn: fqn.clone(),
            owner: None,
            descriptor,
            span: self.line_index.span(self.source, node),
            selection_span: self.line_index.span(self.source, name_node),
            synthetic: false,
        };
        self.definition_spans
            .insert((name_node.start_byte(), name_node.end_byte()));
        self.symbols.push(symbol);
        self.name_to_ids.entry(name).or_default().push(id);
        if let Some(owner) = owner_fqn {
            self.owner_fixups.push((self.symbols.len() - 1, owner));
        }
        self.collect_local_symbols(node, &fqn, id);
    }

    fn collect_local_symbols(&mut self, node: Node<'a>, owner_fqn: &str, owner_id: SymbolId) {
        let mut descendant_nodes = Vec::new();
        descendants(node, &mut descendant_nodes);

        for descendant in descendant_nodes {
            match descendant.kind() {
                "local_variable_declaration" => {
                    let Some(type_node) = descendant.child_by_field_name("type") else {
                        continue;
                    };
                    let declared_type = text_of(type_node, self.source).to_string();
                    for child in named_children(descendant) {
                        if child.kind() != "variable_declarator" {
                            continue;
                        }
                        let Some(name_node) = child.child_by_field_name("name") else {
                            continue;
                        };
                        self.push_local_symbol(
                            name_node,
                            child,
                            SymbolKind::Local,
                            &declared_type,
                            owner_fqn,
                            owner_id,
                        );
                    }
                }
                "formal_parameter" => {
                    let Some(name_node) = descendant.child_by_field_name("name") else {
                        continue;
                    };
                    let declared_type = descendant
                        .child_by_field_name("type")
                        .map(|type_node| text_of(type_node, self.source).to_string())
                        .unwrap_or_default();
                    self.push_local_symbol(
                        name_node,
                        descendant,
                        SymbolKind::Parameter,
                        &declared_type,
                        owner_fqn,
                        owner_id,
                    );
                }
                "catch_formal_parameter" => {
                    let Some(name_node) = descendant.child_by_field_name("name") else {
                        continue;
                    };
                    let declared_type = descendant
                        .named_children(&mut descendant.walk())
                        .find(|child| child.kind() == "catch_type")
                        .map(|catch_type| text_of(catch_type, self.source).to_string())
                        .unwrap_or_default();
                    self.push_local_symbol(
                        name_node,
                        descendant,
                        SymbolKind::Local,
                        &declared_type,
                        owner_fqn,
                        owner_id,
                    );
                }
                _ => {}
            }
        }
    }

    fn push_local_symbol(
        &mut self,
        name_node: Node<'a>,
        span_node: Node<'a>,
        kind: SymbolKind,
        descriptor: &str,
        owner_fqn: &str,
        owner_id: SymbolId,
    ) {
        let name = text_of(name_node, self.source).to_string();
        let fqn = format!("{owner_fqn}::{name}#{}", name_node.start_byte());
        let id = symbol_id(self.file, &fqn, descriptor);
        let symbol = Symbol {
            id,
            file_id: self.file,
            kind,
            name: name.clone(),
            fqn,
            owner: Some(owner_id),
            descriptor: descriptor.to_string(),
            span: self.line_index.span(self.source, span_node),
            selection_span: self.line_index.span(self.source, name_node),
            synthetic: false,
        };
        self.definition_spans
            .insert((name_node.start_byte(), name_node.end_byte()));
        self.symbols.push(symbol);
        self.name_to_ids.entry(name).or_default().push(id);
    }

    fn visit_field_declaration(&mut self, node: Node<'a>) {
        let Some(type_node) = node.child_by_field_name("type") else {
            return;
        };
        let descriptor = text_of(type_node, self.source).to_string();
        for child in named_children(node) {
            if child.kind() != "variable_declarator" {
                continue;
            }
            let Some(name_node) = child.child_by_field_name("name") else {
                continue;
            };
            let name = text_of(name_node, self.source).to_string();
            let owner_fqn = self.current_type_fqn();
            let fqn = match &owner_fqn {
                Some(owner) => format!("{owner}.{name}"),
                None => name.clone(),
            };
            let id = symbol_id(self.file, &fqn, &descriptor);
            let symbol = Symbol {
                id,
                file_id: self.file,
                kind: SymbolKind::Field,
                name: name.clone(),
                fqn: fqn.clone(),
                owner: None,
                descriptor: descriptor.clone(),
                span: self.line_index.span(self.source, child),
                selection_span: self.line_index.span(self.source, name_node),
                synthetic: false,
            };
            self.definition_spans
                .insert((name_node.start_byte(), name_node.end_byte()));
            self.symbols.push(symbol);
            self.name_to_ids.entry(name).or_default().push(id);
            if let Some(owner) = owner_fqn {
                self.owner_fixups.push((self.symbols.len() - 1, owner));
            }
        }
    }

    fn visit_enum_constant(&mut self, node: Node<'a>) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = text_of(name_node, self.source).to_string();
        let owner_fqn = self.current_type_fqn();
        let fqn = match &owner_fqn {
            Some(owner) => format!("{owner}.{name}"),
            None => name.clone(),
        };
        let id = symbol_id(self.file, &fqn, "");
        let symbol = Symbol {
            id,
            file_id: self.file,
            kind: SymbolKind::Field,
            name: name.clone(),
            fqn: fqn.clone(),
            owner: None,
            descriptor: String::new(),
            span: self.line_index.span(self.source, node),
            selection_span: self.line_index.span(self.source, name_node),
            synthetic: false,
        };
        self.definition_spans
            .insert((name_node.start_byte(), name_node.end_byte()));
        self.symbols.push(symbol);
        self.name_to_ids.entry(name).or_default().push(id);
        if let Some(owner) = owner_fqn {
            self.owner_fixups.push((self.symbols.len() - 1, owner));
        }
    }

    fn current_type_fqn(&self) -> Option<String> {
        self.type_stack.last().map(|name| {
            qualified(
                &self.package,
                &self.type_stack[..self.type_stack.len() - 1],
                name,
            )
        })
    }

    fn finish(mut self) -> (FileContribution, HashSet<(usize, usize)>) {
        for (index, owner_fqn) in self.owner_fixups {
            if let Some(owner_id) = self.fqn_to_id.get(&owner_fqn) {
                if let Some(symbol) = self.symbols.get_mut(index) {
                    symbol.owner = Some(*owner_id);
                }
            }
        }

        let mut contribution = FileContribution::new(self.file);
        contribution.symbols = self.symbols;
        contribution.unresolved_type_edges = self.unresolved_type_edges;
        for symbol in &contribution.symbols {
            contribution.occurrences.push(Occurrence {
                symbol_id: symbol.id,
                file_id: self.file,
                span: symbol.selection_span,
                role: OccurrenceRole::Definition,
            });
        }
        (contribution, self.definition_spans)
    }
}

// ---------------------------------------------------------------- references

struct TypeEnvironmentWalker<'a> {
    source: &'a str,
    variable_types: HashMap<String, String>,
}

impl<'a> TypeEnvironmentWalker<'a> {
    fn walk(&mut self, node: Node<'a>) {
        match node.kind() {
            "field_declaration" | "local_variable_declaration" => {
                if let Some(type_node) = node.child_by_field_name("type") {
                    let declared_type = text_of(type_node, self.source).to_string();
                    for child in named_children(node) {
                        if child.kind() != "variable_declarator" {
                            continue;
                        }
                        if let Some(name_node) = child.child_by_field_name("name") {
                            let name = text_of(name_node, self.source).to_string();
                            self.variable_types.insert(name, declared_type.clone());
                        }
                    }
                }
            }
            "formal_parameter" => {
                if let (Some(type_node), Some(name_node)) = (
                    node.child_by_field_name("type"),
                    node.child_by_field_name("name"),
                ) {
                    let declared_type = text_of(type_node, self.source).to_string();
                    let name = text_of(name_node, self.source).to_string();
                    self.variable_types.insert(name, declared_type);
                }
            }
            _ => {}
        }

        for child in named_children(node) {
            self.walk(child);
        }
    }
}

struct ReferenceWalker<'a> {
    file: FileId,
    source: &'a str,
    line_index: &'a LineIndex,
    name_to_ids: HashMap<String, Vec<SymbolId>>,
    variable_types: HashMap<String, String>,
    definition_spans: HashSet<(usize, usize)>,
}

impl<'a> ReferenceWalker<'a> {
    fn walk(&self, node: Node<'a>, contribution: &mut FileContribution) {
        match node.kind() {
            "package_declaration" | "import_declaration" => return,
            "type_identifier" => {
                self.visit_reference(
                    node,
                    contribution,
                    Some(SymbolKind::Class),
                    OccurrenceRole::Type,
                    None,
                    None,
                );
            }
            "identifier" => {
                let text = text_of(node, self.source);
                if self.variable_types.contains_key(text) {
                    self.visit_variable_reference(node, contribution);
                    return;
                }

                if let Some(parent) = node.parent() {
                    if parent.kind() == "method_invocation"
                        && parent
                            .child_by_field_name("object")
                            .is_some_and(|object| object.byte_range() == node.byte_range())
                        && text.chars().next().is_some_and(char::is_uppercase)
                    {
                        self.visit_reference(
                            node,
                            contribution,
                            Some(SymbolKind::Class),
                            OccurrenceRole::Type,
                            None,
                            None,
                        );
                        return;
                    }

                    if parent.kind() == "method_invocation"
                        && parent
                            .child_by_field_name("name")
                            .is_some_and(|name| name.byte_range() == node.byte_range())
                    {
                        let receiver_type = parent.child_by_field_name("object").map(|object| {
                            let receiver_text = text_of(object, self.source);
                            self.variable_types
                                .get(receiver_text)
                                .cloned()
                                .unwrap_or_else(|| receiver_text.to_string())
                        });
                        let argument_count = parent
                            .child_by_field_name("arguments")
                            .map(|arguments| arguments.named_child_count());
                        self.visit_reference(
                            node,
                            contribution,
                            Some(SymbolKind::Method),
                            OccurrenceRole::Call,
                            receiver_type,
                            argument_count,
                        );
                        return;
                    }
                    if parent.kind() == "field_access"
                        && parent
                            .child_by_field_name("field")
                            .is_some_and(|field| field.byte_range() == node.byte_range())
                    {
                        let receiver_type = parent
                            .child_by_field_name("object")
                            .map(|object| text_of(object, self.source).to_string());
                        self.visit_reference(
                            node,
                            contribution,
                            Some(SymbolKind::Field),
                            OccurrenceRole::Read,
                            receiver_type,
                            None,
                        );
                        return;
                    }
                }
            }
            _ => {}
        }

        for child in named_children(node) {
            self.walk(child, contribution);
        }
    }

    fn visit_variable_reference(&self, node: Node<'a>, contribution: &mut FileContribution) {
        if self
            .definition_spans
            .contains(&(node.start_byte(), node.end_byte()))
        {
            return;
        }

        let name = text_of(node, self.source).to_string();
        let span = self.line_index.span(self.source, node);

        let symbol_id = self
            .local_symbol_id(contribution, &name, node)
            .or_else(|| self.field_symbol_id(contribution, &name));
        if let Some(symbol_id) = symbol_id {
            contribution.occurrences.push(Occurrence {
                symbol_id,
                file_id: self.file,
                span,
                role: OccurrenceRole::Read,
            });
        } else {
            contribution.unresolved.push(UnresolvedOccurrence {
                file_id: self.file,
                span,
                name,
                kind_hint: None,
                role: OccurrenceRole::Read,
                receiver_type: None,
                argument_count: None,
            });
        }
    }

    fn local_symbol_id(
        &self,
        contribution: &FileContribution,
        name: &str,
        node: Node<'a>,
    ) -> Option<SymbolId> {
        let current_position = self.line_index.position(self.source, node.start_byte());
        contribution
            .symbols
            .iter()
            .filter(|symbol| {
                symbol.name == name
                    && matches!(symbol.kind, SymbolKind::Local | SymbolKind::Parameter)
            })
            .filter(|symbol| symbol.selection_span.start <= current_position)
            .max_by_key(|symbol| symbol.selection_span.start)
            .map(|symbol| symbol.id)
    }

    fn field_symbol_id(&self, contribution: &FileContribution, name: &str) -> Option<SymbolId> {
        self.name_to_ids.get(name)?.iter().copied().find(|id| {
            contribution
                .symbols
                .iter()
                .any(|symbol| symbol.id == *id && symbol.kind == SymbolKind::Field)
        })
    }

    fn visit_reference(
        &self,
        node: Node<'a>,
        contribution: &mut FileContribution,
        kind_hint: Option<SymbolKind>,
        role: OccurrenceRole,
        receiver_type: Option<String>,
        argument_count: Option<usize>,
    ) {
        if self
            .definition_spans
            .contains(&(node.start_byte(), node.end_byte()))
        {
            return;
        }
        let name = text_of(node, self.source).to_string();
        let span = self.line_index.span(self.source, node);
        let symbol_id = if kind_hint == Some(SymbolKind::Method) {
            match receiver_type.as_deref() {
                Some(receiver) => {
                    let receiver_simple = simple_type_name(receiver);
                    contribution
                        .symbols
                        .iter()
                        .filter(|symbol| symbol.name == name && symbol.kind.is_method())
                        .find(|symbol| method_owner_simple_name(symbol) == Some(receiver_simple))
                        .map(|symbol| symbol.id)
                }
                None => self
                    .name_to_ids
                    .get(&name)
                    .and_then(|ids| ids.first().copied()),
            }
        } else {
            self.name_to_ids
                .get(&name)
                .and_then(|ids| ids.first().copied())
        };
        if let Some(symbol_id) = symbol_id {
            contribution.occurrences.push(Occurrence {
                symbol_id,
                file_id: self.file,
                span,
                role,
            });
        } else {
            contribution.unresolved.push(UnresolvedOccurrence {
                file_id: self.file,
                span,
                name,
                kind_hint,
                role,
                receiver_type,
                argument_count,
            });
        }

        for child in named_children(node) {
            self.walk(child, contribution);
        }
    }
}
