use nav_next::{
    FileContribution, FileId, IndexBuilder, Occurrence, OccurrenceRole, Position, QueryEngine,
    Span, Symbol, SymbolId, SymbolKind, TypeEdge, TypeEdgeKind, UnresolvedOccurrence,
};

fn span(line: u32, start: u32, end: u32) -> Span {
    Span {
        start: Position {
            line,
            character: start,
        },
        end: Position {
            line,
            character: end,
        },
    }
}

fn symbol(
    id: u64,
    file: u32,
    kind: SymbolKind,
    name: &str,
    fqn: &str,
    owner: Option<u64>,
    descriptor: &str,
) -> Symbol {
    Symbol {
        id: SymbolId(id),
        file_id: FileId(file),
        kind,
        name: name.to_string(),
        fqn: fqn.to_string(),
        owner: owner.map(SymbolId),
        descriptor: descriptor.to_string(),
        span: span(1, 0, 20),
        selection_span: span(1, 10, 20),
        synthetic: false,
    }
}

fn direct_interface_and_implementation() -> (FileContribution, FileContribution) {
    let mut interface = FileContribution::new(FileId(1));
    interface.symbols.push(symbol(
        1,
        1,
        SymbolKind::Interface,
        "UserService",
        "demo.UserService",
        None,
        "",
    ));
    interface.symbols.push(symbol(
        2,
        1,
        SymbolKind::Method,
        "findById",
        "demo.UserService.findById",
        Some(1),
        "(int)",
    ));
    interface.occurrences.push(Occurrence {
        symbol_id: SymbolId(2),
        file_id: FileId(1),
        span: span(2, 10, 20),
        role: OccurrenceRole::Definition,
    });

    let mut implementation = FileContribution::new(FileId(2));
    implementation.symbols.push(symbol(
        3,
        2,
        SymbolKind::Class,
        "UserServiceImpl",
        "demo.UserServiceImpl",
        None,
        "",
    ));
    implementation.symbols.push(symbol(
        4,
        2,
        SymbolKind::Method,
        "findById",
        "demo.UserServiceImpl.findById",
        Some(3),
        "(int)",
    ));
    implementation.type_edges.push(TypeEdge {
        child: SymbolId(3),
        parent: SymbolId(1),
        kind: TypeEdgeKind::Implements,
    });
    implementation.occurrences.push(Occurrence {
        symbol_id: SymbolId(4),
        file_id: FileId(2),
        span: span(3, 10, 20),
        role: OccurrenceRole::Definition,
    });

    (interface, implementation)
}

#[test]
fn direct_implementation_and_supertype_round_trip() {
    let (interface, implementation) = direct_interface_and_implementation();
    let mut builder = IndexBuilder::new();
    builder.add_file(interface).unwrap();
    builder.add_file(implementation).unwrap();
    let snapshot = builder.build().unwrap();
    let query = QueryEngine::new(snapshot);

    let implementations = query.implementations(SymbolId(2));
    assert_eq!(implementations.len(), 1);
    assert_eq!(implementations[0].id, SymbolId(4));

    let supertypes = query.supertypes(SymbolId(4));
    assert_eq!(supertypes.len(), 1);
    assert_eq!(supertypes[0].id, SymbolId(2));

    let definitions = query.definition(SymbolId(2));
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].file_id, FileId(1));
}

#[test]
fn transitive_implementation_traverses_intermediate_interface() {
    let mut root = FileContribution::new(FileId(1));
    root.symbols.push(symbol(
        1,
        1,
        SymbolKind::Interface,
        "UserService",
        "demo.UserService",
        None,
        "",
    ));
    root.symbols.push(symbol(
        2,
        1,
        SymbolKind::Method,
        "findById",
        "demo.UserService.findById",
        Some(1),
        "(int)",
    ));

    let mut middle = FileContribution::new(FileId(2));
    middle.symbols.push(symbol(
        3,
        2,
        SymbolKind::Interface,
        "InternalUserService",
        "demo.InternalUserService",
        None,
        "",
    ));
    middle.type_edges.push(TypeEdge {
        child: SymbolId(3),
        parent: SymbolId(1),
        kind: TypeEdgeKind::Extends,
    });

    let mut implementation = FileContribution::new(FileId(3));
    implementation.symbols.push(symbol(
        4,
        3,
        SymbolKind::Class,
        "UserServiceImpl",
        "demo.UserServiceImpl",
        None,
        "",
    ));
    implementation.symbols.push(symbol(
        5,
        3,
        SymbolKind::Method,
        "findById",
        "demo.UserServiceImpl.findById",
        Some(4),
        "(int)",
    ));
    implementation.type_edges.push(TypeEdge {
        child: SymbolId(4),
        parent: SymbolId(3),
        kind: TypeEdgeKind::Implements,
    });

    let mut builder = IndexBuilder::new();
    builder.add_file(root).unwrap();
    builder.add_file(middle).unwrap();
    builder.add_file(implementation).unwrap();
    let snapshot = builder.build().unwrap();
    let query = QueryEngine::new(snapshot);

    let implementations = query.implementations(SymbolId(2));
    assert_eq!(implementations.len(), 1);
    assert_eq!(implementations[0].id, SymbolId(5));

    let supertypes = query.supertypes(SymbolId(5));
    assert_eq!(supertypes.len(), 1);
    assert_eq!(supertypes[0].id, SymbolId(2));
}

#[test]
fn unresolved_call_binds_by_name_and_argument_count() {
    let (interface, implementation) = direct_interface_and_implementation();
    let mut caller = FileContribution::new(FileId(3));
    caller.unresolved.push(UnresolvedOccurrence {
        file_id: FileId(3),
        span: span(4, 5, 15),
        name: "findById".to_string(),
        kind_hint: Some(SymbolKind::Method),
        role: OccurrenceRole::Call,
        receiver_type: Some("UserService".to_string()),
        argument_count: Some(1),
    });

    let mut builder = IndexBuilder::new();
    builder.add_file(interface).unwrap();
    builder.add_file(implementation).unwrap();
    builder.add_file(caller).unwrap();
    let snapshot = builder.build().unwrap();
    let query = QueryEngine::new(snapshot);

    let references = query.references(SymbolId(2));
    assert!(references
        .iter()
        .any(|occurrence| occurrence.file_id == FileId(3) && occurrence.span.start.character == 5));
}

#[test]
fn search_methods_supports_dot_hash_and_ignored_parameters() {
    let (interface, implementation) = direct_interface_and_implementation();
    let mut builder = IndexBuilder::new();
    builder.add_file(interface).unwrap();
    builder.add_file(implementation).unwrap();
    let snapshot = builder.build().unwrap();
    let query = QueryEngine::new(snapshot);

    for input in [
        "demo.UserService#findById(String ignored)",
        "demo.UserService.findById",
        "UserService#findById",
    ] {
        let results = query.search_methods(input, 20);
        assert_eq!(results.len(), 1, "input={input}");
        assert_eq!(results[0].id, SymbolId(2));
    }

    let simple_name_results = query.search_methods("findById", 20);
    assert_eq!(simple_name_results.len(), 2);
    assert!(simple_name_results
        .iter()
        .any(|symbol| symbol.id == SymbolId(2)));
    assert!(simple_name_results
        .iter()
        .any(|symbol| symbol.id == SymbolId(4)));
}

#[test]
fn builder_rejects_duplicate_file_and_symbol() {
    let (interface, implementation) = direct_interface_and_implementation();
    let mut builder = IndexBuilder::new();
    builder.add_file(interface.clone()).unwrap();
    assert!(builder.add_file(interface.clone()).is_err());

    let mut duplicate_symbol = FileContribution::new(FileId(9));
    duplicate_symbol.symbols.push(symbol(
        1,
        9,
        SymbolKind::Class,
        "Duplicate",
        "demo.Duplicate",
        None,
        "",
    ));
    let mut builder = IndexBuilder::new();
    builder.add_file(interface).unwrap();
    builder.add_file(implementation).unwrap();
    builder.add_file(duplicate_symbol).unwrap();
    assert!(builder.build().is_err());
}
