use nav_next::{
    hash_content, ContentError, ContentSource, ContentSourceKind, DocumentStore, FileChange,
    FileChangeDetector, FileFingerprint, FileId, PlainTextHash, StaticContentSource,
};

fn fingerprint(file: u32, size: u64, mtime: u128, text: &str) -> FileFingerprint {
    FileFingerprint {
        file_id: FileId(file),
        size,
        mtime_ns: mtime,
        hash: hash_content(text),
    }
}

#[test]
fn same_plaintext_hash_is_stable_and_different_text_changes() {
    let first = hash_content("class A {}");
    let second = hash_content("class A {}");
    let third = hash_content("class B {}");

    assert_eq!(first, second);
    assert_ne!(first, third);
    assert_ne!(PlainTextHash([0; 32]), first);
}

#[test]
fn document_store_tracks_text_version_and_hash() {
    let mut store = DocumentStore::new();
    let file = FileId(1);

    let first_hash = store.open(file, "class A {}", 1).unwrap();
    assert_eq!(store.text(file).unwrap().as_ref(), "class A {}");
    assert_eq!(store.version(file).unwrap(), 1);
    assert_eq!(store.hash(file).unwrap(), first_hash);

    let second_hash = store.update(file, "class B {}", 2).unwrap();
    assert_ne!(first_hash, second_hash);
    assert_eq!(store.text(file).unwrap().as_ref(), "class B {}");
    assert_eq!(store.version(file).unwrap(), 2);
    assert_eq!(store.hash(file).unwrap(), second_hash);

    let closed = store.close(file).unwrap();
    assert_eq!(closed.text.as_ref(), "class B {}");
    assert_eq!(closed.version, 2);
    assert!(store.entry(file).is_none());
}

#[test]
fn document_store_rejects_invalid_open_update_and_close() {
    let mut store = DocumentStore::new();
    let file = FileId(7);

    assert_eq!(
        store.update(file, "text", 1).unwrap_err(),
        ContentError::NotOpen(file)
    );
    store.open(file, "text", 1).unwrap();
    assert_eq!(
        store.open(file, "text", 2).unwrap_err(),
        ContentError::AlreadyOpen(file)
    );
    store.close(file).unwrap();
    assert_eq!(store.close(file).unwrap_err(), ContentError::NotOpen(file));
}

#[test]
fn document_store_is_an_editor_content_source() {
    let mut store = DocumentStore::new();
    let file = FileId(3);
    store.open(file, "package demo;", 1).unwrap();

    assert_eq!(store.kind(), ContentSourceKind::Editor);
    assert_eq!(store.read(file).unwrap().as_ref(), "package demo;");

    store.close(file).unwrap();
    assert_eq!(store.read(file).unwrap_err(), ContentError::NotOpen(file));
}

#[test]
fn static_content_source_reads_inserted_files() {
    let mut source = StaticContentSource::new();
    let file = FileId(11);
    assert_eq!(source.read(file).unwrap_err(), ContentError::Missing(file));

    source.insert(file, "class C {}");
    assert_eq!(source.kind(), ContentSourceKind::Static);
    assert_eq!(source.read(file).unwrap().as_ref(), "class C {}");

    source.remove(file);
    assert_eq!(source.read(file).unwrap_err(), ContentError::Missing(file));
}

#[test]
fn file_change_detector_uses_plaintext_hash_as_authority() {
    let mut detector = FileChangeDetector::new();
    let file = FileId(1);

    assert_eq!(
        detector.observe(fingerprint(1, 10, 100, "one")),
        FileChange::New
    );
    assert_eq!(
        detector.observe(fingerprint(1, 11, 101, "one")),
        FileChange::MetadataOnly
    );
    assert_eq!(
        detector.observe(fingerprint(1, 11, 101, "one")),
        FileChange::Unchanged
    );
    assert_eq!(
        detector.observe(fingerprint(1, 11, 101, "two")),
        FileChange::ContentChanged
    );

    assert_eq!(detector.current(file).unwrap().hash, hash_content("two"));
    assert_eq!(detector.remove(file), FileChange::Missing);
    assert!(detector.current(file).is_none());
    assert_eq!(detector.remove(file), FileChange::Unchanged);
}
