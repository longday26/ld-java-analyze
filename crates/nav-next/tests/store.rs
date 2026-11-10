use nav_next::{
    hash_content, FileContribution, FileFingerprint, FileId, IndexStore, Position, QueryEngine,
    Span, StoredFile, Symbol, SymbolId, SymbolKind, TypeEdge, TypeEdgeKind,
};
use tempfile::TempDir;

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
) -> Symbol {
    Symbol {
        id: SymbolId(id),
        file_id: FileId(file),
        kind,
        name: name.to_string(),
        fqn: fqn.to_string(),
        owner: owner.map(SymbolId),
        descriptor: String::new(),
        span: span(1, 0, 20),
        selection_span: span(1, 10, 20),
        synthetic: false,
    }
}

fn stored_interface() -> StoredFile {
    let mut contribution = FileContribution::new(FileId(1));
    contribution.symbols.push(symbol(
        1,
        1,
        SymbolKind::Interface,
        "UserService",
        "demo.UserService",
        None,
    ));
    contribution.symbols.push(symbol(
        2,
        1,
        SymbolKind::Method,
        "findById",
        "demo.UserService.findById",
        Some(1),
    ));

    StoredFile {
        path: "interface/UserService.java".into(),
        contribution,
        fingerprint: FileFingerprint {
            file_id: FileId(1),
            size: 100,
            mtime_ns: 1_000,
            hash: hash_content("interface"),
        },
    }
}

fn stored_implementation() -> StoredFile {
    let mut contribution = FileContribution::new(FileId(2));
    contribution.symbols.push(symbol(
        3,
        2,
        SymbolKind::Class,
        "UserServiceImpl",
        "demo.UserServiceImpl",
        None,
    ));
    contribution.symbols.push(symbol(
        4,
        2,
        SymbolKind::Method,
        "findById",
        "demo.UserServiceImpl.findById",
        Some(3),
    ));
    contribution.type_edges.push(TypeEdge {
        child: SymbolId(3),
        parent: SymbolId(1),
        kind: TypeEdgeKind::Implements,
    });

    StoredFile {
        path: "impl/UserServiceImpl.java".into(),
        contribution,
        fingerprint: FileFingerprint {
            file_id: FileId(2),
            size: 120,
            mtime_ns: 2_000,
            hash: hash_content("implementation"),
        },
    }
}

#[test]
fn store_persists_and_reloads_files() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("index.redb");
    let interface = stored_interface();
    let implementation = stored_implementation();

    {
        let store = IndexStore::open(&path).unwrap();
        store.save_file(&interface).unwrap();
        store.save_file(&implementation).unwrap();
        assert_eq!(store.file_count().unwrap(), 2);
    }

    let store = IndexStore::open(&path).unwrap();
    let records = store.load_files().unwrap();
    assert_eq!(records.len(), 2);
    assert!(records.contains(&interface));
    assert!(records.contains(&implementation));

    store.remove_file(FileId(1)).unwrap();
    assert_eq!(store.file_count().unwrap(), 1);
    let remaining = store.load_files().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].file_id(), FileId(2));
}

#[test]
fn store_rebuilds_snapshot_and_supports_queries() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("index.redb");
    {
        let store = IndexStore::open(&path).unwrap();
        store.save_file(&stored_interface()).unwrap();
        store.save_file(&stored_implementation()).unwrap();
    }

    let store = IndexStore::open(&path).unwrap();
    let snapshot = store.load_snapshot().unwrap();
    let query = QueryEngine::new(snapshot);

    assert_eq!(query.snapshot().file_count(), 2);
    assert_eq!(query.snapshot().symbol_count(), 4);
    let implementations = query.implementations(SymbolId(2));
    assert_eq!(implementations.len(), 1);
    assert_eq!(implementations[0].id, SymbolId(4));
    let supertypes = query.supertypes(SymbolId(4));
    assert_eq!(supertypes.len(), 1);
    assert_eq!(supertypes[0].id, SymbolId(2));
}
