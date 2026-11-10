//! Clean-room navigation model for the next-generation index.
//!
//! This crate intentionally does not reuse `nav-index`. It defines the stable
//! contracts needed by the future incremental indexing pipeline:
//!
//! - `FileId` / `SymbolId` / `ShardId` are explicit identities.
//! - `FileContribution` remains the per-file unit of parsed truth.
//! - `IndexBuilder` is the single writer that turns contributions into an
//!   immutable `IndexSnapshot`.
//! - `QueryEngine` is read-only and never mutates index state.

pub mod content;
pub mod index;
pub mod java;
pub mod model;
pub mod snapshot;
pub mod store;
pub mod workspace;

pub use content::{
    hash_content, ContentError, ContentSource, ContentSourceKind, DocumentEntry, DocumentStore,
    FileChange, FileChangeDetector, FileFingerprint, PlainTextHash, StaticContentSource,
};
pub use index::{IndexBuilder, IndexSnapshot, Location, QueryEngine};
pub use java::JavaAnalyzer;
pub use model::{
    FileContribution, FileId, Link, LinkEndpoint, Occurrence, OccurrenceRole, Position, ShardId,
    Span, Symbol, SymbolId, SymbolKind, TypeEdge, TypeEdgeKind, UnresolvedOccurrence,
    UnresolvedTypeEdge,
};
pub use snapshot::SnapshotStore;
pub use store::{IndexStore, StoreError, StoredFile};
pub use workspace::WorkspaceFiles;
