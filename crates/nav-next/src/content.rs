//! Content access, editor document state, and plaintext change fingerprints.
//!
//! The next-generation index never trusts ciphertext bytes as a semantic
//! change signal. A file is considered changed only when its plaintext hash
//! changes.

use crate::model::FileId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Stable 256-bit plaintext fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlainTextHash(pub [u8; 32]);

/// Computes the authoritative plaintext hash for editor or bridge content.
pub fn hash_content(text: &str) -> PlainTextHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(text.as_bytes());
    let hash = hasher.finalize();
    PlainTextHash(*hash.as_bytes())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentError {
    AlreadyOpen(FileId),
    NotOpen(FileId),
    Missing(FileId),
    Io(String),
    Bridge(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentSourceKind {
    Editor,
    Disk,
    Bridge,
    Static,
}

pub trait ContentSource {
    fn read(&self, file: FileId) -> Result<Arc<str>, ContentError>;
    fn kind(&self) -> ContentSourceKind;
}

#[derive(Debug, Clone)]
pub struct DocumentEntry {
    pub text: Arc<str>,
    pub version: u64,
    pub hash: PlainTextHash,
}

/// In-memory state for the small set of currently open documents.
///
/// The target workload keeps fewer than 100 documents open, so the store can
/// retain full plaintext text without a paging strategy.
#[derive(Debug, Default)]
pub struct DocumentStore {
    entries: HashMap<FileId, DocumentEntry>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(
        &mut self,
        file: FileId,
        text: impl Into<Arc<str>>,
        version: u64,
    ) -> Result<PlainTextHash, ContentError> {
        if self.entries.contains_key(&file) {
            return Err(ContentError::AlreadyOpen(file));
        }
        let text = text.into();
        let hash = hash_content(&text);
        self.entries.insert(
            file,
            DocumentEntry {
                text,
                version,
                hash,
            },
        );
        Ok(hash)
    }

    pub fn update(
        &mut self,
        file: FileId,
        text: impl Into<Arc<str>>,
        version: u64,
    ) -> Result<PlainTextHash, ContentError> {
        if !self.entries.contains_key(&file) {
            return Err(ContentError::NotOpen(file));
        }
        let text = text.into();
        let hash = hash_content(&text);
        self.entries.insert(
            file,
            DocumentEntry {
                text,
                version,
                hash,
            },
        );
        Ok(hash)
    }

    pub fn close(&mut self, file: FileId) -> Result<DocumentEntry, ContentError> {
        self.entries
            .remove(&file)
            .ok_or(ContentError::NotOpen(file))
    }

    pub fn entry(&self, file: FileId) -> Option<&DocumentEntry> {
        self.entries.get(&file)
    }

    pub fn text(&self, file: FileId) -> Option<Arc<str>> {
        self.entries.get(&file).map(|entry| entry.text.clone())
    }

    pub fn hash(&self, file: FileId) -> Option<PlainTextHash> {
        self.entries.get(&file).map(|entry| entry.hash)
    }

    pub fn version(&self, file: FileId) -> Option<u64> {
        self.entries.get(&file).map(|entry| entry.version)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl ContentSource for DocumentStore {
    fn read(&self, file: FileId) -> Result<Arc<str>, ContentError> {
        self.text(file).ok_or(ContentError::NotOpen(file))
    }

    fn kind(&self) -> ContentSourceKind {
        ContentSourceKind::Editor
    }
}

/// Simple in-memory source used by tests and future fallback adapters.
#[derive(Debug, Default)]
pub struct StaticContentSource {
    files: HashMap<FileId, Arc<str>>,
}

impl StaticContentSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, file: FileId, text: impl Into<Arc<str>>) {
        self.files.insert(file, text.into());
    }

    pub fn remove(&mut self, file: FileId) {
        self.files.remove(&file);
    }
}

impl ContentSource for StaticContentSource {
    fn read(&self, file: FileId) -> Result<Arc<str>, ContentError> {
        self.files
            .get(&file)
            .cloned()
            .ok_or(ContentError::Missing(file))
    }

    fn kind(&self) -> ContentSourceKind {
        ContentSourceKind::Static
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub file_id: FileId,
    pub size: u64,
    pub mtime_ns: u128,
    pub hash: PlainTextHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileChange {
    New,
    Unchanged,
    MetadataOnly,
    ContentChanged,
    Missing,
}

/// Tracks authoritative plaintext changes per file.
///
/// `stat` differences alone do not mean the Java source changed; they only
/// trigger a new plaintext read. The stored fingerprint always reflects the
/// latest observation.
#[derive(Debug, Default)]
pub struct FileChangeDetector {
    fingerprints: HashMap<FileId, FileFingerprint>,
}

impl FileChangeDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, fingerprint: FileFingerprint) -> FileChange {
        let file = fingerprint.file_id;
        match self.fingerprints.get(&file) {
            None => {
                self.fingerprints.insert(file, fingerprint);
                FileChange::New
            }
            Some(previous) => {
                let change = if previous.hash != fingerprint.hash {
                    FileChange::ContentChanged
                } else if previous.size != fingerprint.size
                    || previous.mtime_ns != fingerprint.mtime_ns
                {
                    FileChange::MetadataOnly
                } else {
                    FileChange::Unchanged
                };
                self.fingerprints.insert(file, fingerprint);
                change
            }
        }
    }

    pub fn remove(&mut self, file: FileId) -> FileChange {
        if self.fingerprints.remove(&file).is_some() {
            FileChange::Missing
        } else {
            FileChange::Unchanged
        }
    }

    pub fn current(&self, file: FileId) -> Option<&FileFingerprint> {
        self.fingerprints.get(&file)
    }

    pub fn len(&self) -> usize {
        self.fingerprints.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fingerprints.is_empty()
    }
}
