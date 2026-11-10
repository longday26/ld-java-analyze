//! Stable data contracts shared by the parser, builder, store, and query engine.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Unique identity of a source file inside one workspace.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct FileId(pub u32);

/// Stable identity of a symbol inside one index generation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct SymbolId(pub u64);

/// Identity of an occurrence shard in the persistent store.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct ShardId(pub u32);

/// A UTF-16 position as used by LSP.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A half-open span in UTF-16 coordinates.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct Span {
    pub start: Position,
    pub end: Position,
}

impl Span {
    pub fn contains(&self, position: Position) -> bool {
        self.start <= position && position < self.end
    }

    pub fn width(&self) -> usize {
        if self.start.line == self.end.line {
            (self.end.character - self.start.character) as usize
        } else {
            0
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SymbolKind {
    Class,
    Interface,
    Enum,
    Annotation,
    Record,
    Method,
    Constructor,
    Field,
    Local,
    Parameter,
    Synthetic,
}

impl SymbolKind {
    pub fn is_type(self) -> bool {
        matches!(
            self,
            Self::Class | Self::Interface | Self::Enum | Self::Annotation | Self::Record
        )
    }

    pub fn is_method(self) -> bool {
        matches!(self, Self::Method | Self::Constructor | Self::Synthetic)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: SymbolId,
    pub file_id: FileId,
    pub kind: SymbolKind,
    pub name: String,
    pub fqn: String,
    pub owner: Option<SymbolId>,
    pub descriptor: String,
    pub span: Span,
    pub selection_span: Span,
    pub synthetic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum OccurrenceRole {
    Definition,
    Reference,
    Read,
    Write,
    Call,
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub symbol_id: SymbolId,
    pub file_id: FileId,
    pub span: Span,
    pub role: OccurrenceRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedOccurrence {
    pub file_id: FileId,
    pub span: Span,
    pub name: String,
    pub kind_hint: Option<SymbolKind>,
    pub role: OccurrenceRole,
    pub receiver_type: Option<String>,
    pub argument_count: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TypeEdgeKind {
    Extends,
    Implements,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeEdge {
    pub child: SymbolId,
    pub parent: SymbolId,
    pub kind: TypeEdgeKind,
}

/// A `extends` / `implements` edge whose parent symbol is not known while
/// parsing a single file. The index builder resolves it after all files are
/// available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedTypeEdge {
    pub child: SymbolId,
    pub parent_name: String,
    pub kind: TypeEdgeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkEndpoint {
    pub file_id: FileId,
    pub span: Span,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub from: LinkEndpoint,
    pub to: LinkEndpoint,
    pub kind: String,
    pub bidirectional: bool,
}

/// The per-file unit of parsed truth.
///
/// The builder owns all cross-file state; a contribution never references
/// mutable global state.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FileContribution {
    pub file_id: FileId,
    pub symbols: Vec<Symbol>,
    pub occurrences: Vec<Occurrence>,
    pub unresolved: Vec<UnresolvedOccurrence>,
    pub type_edges: Vec<TypeEdge>,
    pub unresolved_type_edges: Vec<UnresolvedTypeEdge>,
    pub links: Vec<Link>,
}

impl FileContribution {
    pub fn new(file_id: FileId) -> Self {
        Self {
            file_id,
            ..Default::default()
        }
    }

    pub fn symbol_ids(&self) -> HashSet<SymbolId> {
        self.symbols.iter().map(|symbol| symbol.id).collect()
    }
}
