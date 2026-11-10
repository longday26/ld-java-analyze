//! Embedded persistent storage for per-file contributions and fingerprints.

use crate::content::FileFingerprint;
use crate::index::{IndexBuilder, IndexSnapshot};
use crate::model::{FileContribution, FileId};
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const FILES: redb::TableDefinition<u32, &[u8]> = redb::TableDefinition::new("files");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredFile {
    pub path: PathBuf,
    pub contribution: FileContribution,
    pub fingerprint: FileFingerprint,
}

impl StoredFile {
    pub fn file_id(&self) -> FileId {
        self.contribution.file_id
    }
}

#[derive(Debug)]
pub enum StoreError {
    Redb(redb::Error),
    Postcard(postcard::Error),
    Io(std::io::Error),
    Build(String),
}

impl From<redb::Error> for StoreError {
    fn from(error: redb::Error) -> Self {
        Self::Redb(error)
    }
}

impl From<redb::DatabaseError> for StoreError {
    fn from(error: redb::DatabaseError) -> Self {
        Self::Redb(error.into())
    }
}

impl From<redb::TransactionError> for StoreError {
    fn from(error: redb::TransactionError) -> Self {
        Self::Redb(error.into())
    }
}

impl From<redb::TableError> for StoreError {
    fn from(error: redb::TableError) -> Self {
        Self::Redb(error.into())
    }
}

impl From<redb::StorageError> for StoreError {
    fn from(error: redb::StorageError) -> Self {
        Self::Redb(error.into())
    }
}

impl From<redb::CommitError> for StoreError {
    fn from(error: redb::CommitError) -> Self {
        Self::Redb(error.into())
    }
}

impl From<postcard::Error> for StoreError {
    fn from(error: postcard::Error) -> Self {
        Self::Postcard(error)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Redb(error) => write!(f, "redb error: {error}"),
            Self::Postcard(error) => write!(f, "postcard error: {error}"),
            Self::Io(error) => write!(f, "io error: {error}"),
            Self::Build(error) => write!(f, "index build error: {error}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Redb(error) => Some(error),
            Self::Postcard(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Build(_) => None,
        }
    }
}

/// redb-backed index store.
///
/// The store persists per-file truth and can rebuild an immutable snapshot on
/// startup. This stage intentionally does not persist derived query structures;
/// they are reconstructed deterministically from `FileContribution`.
#[derive(Debug)]
pub struct IndexStore {
    database: redb::Database,
    path: PathBuf,
}

impl IndexStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let database = redb::Database::create(&path)?;
        Ok(Self { database, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn save_file(&self, record: &StoredFile) -> Result<(), StoreError> {
        let bytes = postcard::to_allocvec(record)?;
        let transaction = self.database.begin_write()?;
        {
            let mut table = transaction.open_table(FILES)?;
            table.insert(record.contribution.file_id.0, bytes.as_slice())?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn remove_file(&self, file: FileId) -> Result<(), StoreError> {
        let transaction = self.database.begin_write()?;
        {
            let mut table = transaction.open_table(FILES)?;
            table.remove(file.0)?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn load_files(&self) -> Result<Vec<StoredFile>, StoreError> {
        let transaction = self.database.begin_read()?;
        let table = transaction.open_table(FILES)?;
        let mut records = Vec::new();
        for row in table.iter()? {
            let (_, value) = row?;
            let bytes = value.value();
            let record: StoredFile = postcard::from_bytes(bytes)?;
            records.push(record);
        }
        Ok(records)
    }

    pub fn file_count(&self) -> Result<usize, StoreError> {
        let transaction = self.database.begin_read()?;
        let table = transaction.open_table(FILES)?;
        let count = table.len()?;
        Ok(count as usize)
    }

    /// Rebuilds an immutable snapshot from persisted per-file contributions.
    pub fn load_snapshot(&self) -> Result<Arc<IndexSnapshot>, StoreError> {
        let mut builder = IndexBuilder::new();
        for record in self.load_files()? {
            builder
                .add_file(record.contribution)
                .map_err(StoreError::Build)?;
        }
        builder.build().map_err(StoreError::Build)
    }
}
