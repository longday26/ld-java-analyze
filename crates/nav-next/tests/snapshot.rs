use nav_next::{
    FileContribution, FileId, IndexBuilder, Occurrence, OccurrenceRole, Position, QueryEngine,
    SnapshotStore, Span, Symbol, SymbolId, SymbolKind,
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

fn symbol(id: u64, file: u32, name: &str) -> Symbol {
    Symbol {
        id: SymbolId(id),
        file_id: FileId(file),
        kind: SymbolKind::Class,
        name: name.to_string(),
        fqn: format!("demo.{name}"),
        owner: None,
        descriptor: String::new(),
        span: span(1, 0, 20),
        selection_span: span(1, 10, 20),
        synthetic: false,
    }
}

#[test]
fn snapshot_store_publishes_and_keeps_old_query_engine_stable() {
    let mut builder = IndexBuilder::new();

    let mut first_contribution = FileContribution::new(FileId(1));
    first_contribution.symbols.push(symbol(1, 1, "First"));
    builder.add_file(first_contribution).unwrap();
    let first = builder.build().unwrap();

    let mut second_contribution = FileContribution::new(FileId(2));
    second_contribution.symbols.push(symbol(2, 2, "Second"));
    builder.add_file(second_contribution).unwrap();
    let second = builder.build().unwrap();

    let store = SnapshotStore::new(Some(first));
    let old_query = store.current();
    assert_eq!(old_query.snapshot().symbol_count(), 1);
    assert_eq!(old_query.snapshot().generation(), 1);

    store.publish(second);
    let new_query = store.current();
    assert_eq!(new_query.snapshot().generation(), 2);
    assert_eq!(new_query.snapshot().symbol_count(), 2);

    // The old engine still owns the immutable old snapshot.
    assert_eq!(old_query.snapshot().generation(), 1);
    assert_eq!(old_query.snapshot().symbol_count(), 1);
    assert_eq!(store.generation(), 2);
}

#[test]
fn symbol_at_prefers_smallest_definition_occurrence() {
    let mut contribution = FileContribution::new(FileId(1));
    contribution.symbols.push(symbol(1, 1, "UserService"));

    contribution.occurrences.push(Occurrence {
        symbol_id: SymbolId(1),
        file_id: FileId(1),
        span: span(2, 10, 20),
        role: OccurrenceRole::Reference,
    });
    contribution.occurrences.push(Occurrence {
        symbol_id: SymbolId(1),
        file_id: FileId(1),
        span: span(2, 12, 18),
        role: OccurrenceRole::Definition,
    });

    let mut builder = IndexBuilder::new();
    builder.add_file(contribution).unwrap();
    let snapshot = builder.build().unwrap();
    let query = QueryEngine::new(snapshot);

    assert_eq!(
        query.symbol_at(
            FileId(1),
            Position {
                line: 2,
                character: 14
            }
        ),
        Some(SymbolId(1))
    );
    assert_eq!(
        query.definition_at(
            FileId(1),
            Position {
                line: 2,
                character: 14
            }
        ),
        vec![nav_next::Location {
            file_id: FileId(1),
            span: span(1, 10, 20),
        }]
    );
}
