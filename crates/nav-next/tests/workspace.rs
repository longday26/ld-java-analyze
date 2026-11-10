use nav_next::{FileId, WorkspaceFiles};
use std::path::Path;

#[test]
fn workspace_files_assigns_stable_ids_and_reverse_lookup() {
    let mut files = WorkspaceFiles::new();
    let first = files.register_path("src/main/java/demo/A.java");
    let second = files.register_path("src/main/java/demo/B.java");
    let first_again = files.register_path("src/main/java/demo/A.java");

    assert_eq!(first, first_again);
    assert_ne!(first, second);
    assert_eq!(files.len(), 2);
    assert_eq!(
        files.path_of(first),
        Some(Path::new("src/main/java/demo/A.java"))
    );
    assert_eq!(
        files.path_of(second),
        Some(Path::new("src/main/java/demo/B.java"))
    );
    assert_eq!(
        files.file_id_of(Path::new("src/main/java/demo/A.java")),
        Some(first)
    );
    assert_eq!(
        files.file_id_of(Path::new("src/main/java/demo/B.java")),
        Some(second)
    );
}

#[test]
fn workspace_files_remove_by_path_and_file_id() {
    let mut files = WorkspaceFiles::new();
    let first = files.register_path("a.java");
    let second = files.register_path("b.java");

    assert_eq!(files.remove_path(Path::new("a.java")), Some(first));
    assert!(files.path_of(first).is_none());
    assert!(files.file_id_of(Path::new("a.java")).is_none());

    assert_eq!(files.remove_file(second), Some("b.java".into()));
    assert!(files.path_of(second).is_none());
    assert_eq!(files.len(), 0);
}

#[test]
fn workspace_files_iterates_paths_and_ids() {
    let mut files = WorkspaceFiles::new();
    let first = files.register_path("a.java");
    let second = files.register_path("b.java");

    let mut items: Vec<_> = files
        .iter()
        .map(|(path, id)| (path.to_string_lossy().into_owned(), id))
        .collect();
    items.sort_by_key(|(_, id)| id.0);

    assert_eq!(
        items,
        vec![
            ("a.java".to_string(), FileId(1)),
            ("b.java".to_string(), FileId(2)),
        ]
    );
    assert_eq!(first, FileId(1));
    assert_eq!(second, FileId(2));
}
