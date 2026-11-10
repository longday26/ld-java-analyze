//! Workspace file identity mapping.
//!
//! `FileId` is the stable internal identity used by the index. Paths are only
//! used at the workspace boundary and are never used as symbol identity.

use crate::model::FileId;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct WorkspaceFiles {
    next_file_id: u32,
    by_path: HashMap<PathBuf, FileId>,
    by_id: HashMap<FileId, PathBuf>,
}

impl WorkspaceFiles {
    pub fn new() -> Self {
        Self {
            next_file_id: 1,
            by_path: HashMap::new(),
            by_id: HashMap::new(),
        }
    }

    /// Registers a path and returns its stable `FileId`.
    ///
    /// Registering the same path twice returns the same id. FileId 0 is
    /// reserved as the default/invalid value, so allocation starts at 1.
    pub fn register_path(&mut self, path: impl Into<PathBuf>) -> FileId {
        let path = path.into();
        if let Some(existing) = self.by_path.get(&path) {
            return *existing;
        }
        let id = FileId(self.next_file_id);
        self.next_file_id += 1;
        self.by_path.insert(path.clone(), id);
        self.by_id.insert(id, path);
        id
    }

    pub fn remove_path(&mut self, path: &Path) -> Option<FileId> {
        let id = self.by_path.remove(path)?;
        self.by_id.remove(&id);
        Some(id)
    }

    pub fn remove_file(&mut self, file: FileId) -> Option<PathBuf> {
        let path = self.by_id.remove(&file)?;
        self.by_path.remove(&path);
        Some(path)
    }

    pub fn path_of(&self, file: FileId) -> Option<&Path> {
        self.by_id.get(&file).map(|path| path.as_path())
    }

    pub fn file_id_of(&self, path: &Path) -> Option<FileId> {
        self.by_path.get(path).copied()
    }

    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Path, FileId)> + '_ {
        self.by_path.iter().map(|(path, id)| (path.as_path(), *id))
    }
}
