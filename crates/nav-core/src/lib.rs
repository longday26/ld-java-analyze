//! Shared data model for the Java code navigator.
//!
//! The core index only knows about `Symbol`, `Occurrence` and `Link`.
//! Framework knowledge lives behind [`FrameworkProvider`] implementations.

pub mod scan_rules;

pub use scan_rules::{ExcludeRule, RuleTarget, ScanConfig, ScanRules};

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

pub type SymbolId = u64;

/// Stable within one local index cache; the cache is always rebuildable.
pub fn symbol_id(parts: &[&str]) -> SymbolId {
    let mut hasher = DefaultHasher::new();
    for part in parts {
        part.hash(&mut hasher);
        0xffu8.hash(&mut hasher);
    }
    hasher.finish()
}

/// Converts a UTF-8 byte offset into an LSP position (`character` counts UTF-16 units).
pub fn position_at(source: &str, byte: usize) -> Position {
    let end = byte.min(source.len());
    let line = source.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as u32;
    let line_start = source[..end].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = source[line_start..end].encode_utf16().count() as u32;
    Position { line, character }
}

/// Normalises a filesystem path to a `file://` URI string usable across crates.
pub fn file_uri(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    }
}

/// Converts a `file://` URI back to a filesystem path. Only `%XX` escapes are
/// decoded, which covers what `file_uri` and VS Code produce for local files.
pub fn uri_to_path(uri: &str) -> PathBuf {
    let raw = uri
        .strip_prefix("file:///")
        .or_else(|| uri.strip_prefix("file://"))
        .unwrap_or(uri);
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &raw[index + 1..index + 3];
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                decoded.push(value);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&decoded).replace('/', std::path::MAIN_SEPARATOR_STR))
}

/// Case-insensitive, separator-insensitive path comparison for Windows hosts.
pub fn path_starts_with(path: &Path, prefix: &Path) -> bool {
    let normalize = |value: &Path| {
        value
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_lowercase()
    };
    let (path, prefix) = (normalize(path), normalize(prefix));
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

// ---------------------------------------------------------------- positions

/// Zero-based LSP position, `character` counts UTF-16 code units.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }

    pub fn contains(&self, pos: Position) -> bool {
        let after_start = (pos.line, pos.character) >= (self.start.line, self.start.character);
        let before_end = (pos.line, pos.character) < (self.end.line, self.end.character);
        after_start && before_end
    }
}

// ------------------------------------------------------------------- symbols

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    Class,
    Interface,
    Enum,
    Annotation,
    Method,
    Constructor,
    Field,
    Local,
    Parameter,
    Synthetic,
    XmlStatement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolOrigin {
    Workspace,
    SourceJar,
    CompiledJar,
    Synthetic,
}

/// A definition. `selection_range` points at the name token used for jumps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: SymbolId,
    pub kind: SymbolKind,
    pub name: String,
    pub owner_fqn: Option<String>,
    pub fqn: String,
    pub descriptor: String,
    pub uri: String,
    pub range: Range,
    pub selection_range: Range,
    pub origin: SymbolOrigin,
    pub synthetic: bool,
}

impl Symbol {
    pub fn display_name(&self) -> String {
        if self.descriptor.is_empty() {
            self.name.clone()
        } else {
            format!("{}{}", self.name, self.descriptor)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OccurrenceRole {
    Definition,
    Reference,
    Read,
    Write,
    Call,
    Type,
    Import,
}

/// One appearance of a symbol in a file. Powers references, highlights and usage kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    pub symbol_id: SymbolId,
    pub uri: String,
    pub range: Range,
    pub role: OccurrenceRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkEndpoint {
    pub uri: String,
    pub range: Range,
    pub label: String,
}

/// A cross-file / cross-language navigation edge (e.g. Mapper method <-> XML statement).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub from: LinkEndpoint,
    pub to: LinkEndpoint,
    pub kind: String,
    pub bidirectional: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TypeEdgeKind {
    Extends,
    Implements,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeEdge {
    pub child_fqn: String,
    pub parent_fqn: String,
    pub kind: TypeEdgeKind,
}

/// A reference that could not be bound inside its own file (cross-file call/type usage).
/// The index resolves it once all files are known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UnresolvedOccurrence {
    pub uri: String,
    pub range: Range,
    pub name: String,
    pub kind_hint: Option<SymbolKind>,
    pub role: OccurrenceRole,
    /// Simple type name of the receiver when available, e.g. `userService` field type.
    pub receiver_type: Option<String>,
    /// Number of arguments at a method call site, used to disambiguate overloads.
    pub argument_count: Option<usize>,
}

impl Default for UnresolvedOccurrence {
    fn default() -> Self {
        Self {
            uri: String::new(),
            range: Range::default(),
            name: String::new(),
            kind_hint: None,
            role: OccurrenceRole::Reference,
            receiver_type: None,
            argument_count: None,
        }
    }
}

/// Everything one parsed file contributes to the index.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileContribution {
    pub uri: String,
    pub symbols: Vec<Symbol>,
    pub occurrences: Vec<Occurrence>,
    pub unresolved: Vec<UnresolvedOccurrence>,
    pub links: Vec<Link>,
    pub type_edges: Vec<TypeEdge>,
}

// -------------------------------------------------------------- project model

/// Read-only view of the analysed workspace. Never spawns Maven or touches the network.
#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    pub root: PathBuf,
    /// All state lives here, e.g. `<workspace>/.ld-java-analyze`.
    pub state_dir: PathBuf,
    pub modules: Vec<PathBuf>,
    /// Parsed from a user-generated `.ld-java-analyze/classpath.txt` when present.
    pub classpath: Vec<PathBuf>,
    pub source_jars: Vec<PathBuf>,
    /// `.gitignore`-style directory/file name rules used by every workspace walk.
    pub scan_rules: ScanRules,
}

impl ProjectContext {
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        let state_dir = root.join(".ld-java-analyze");
        Self {
            root,
            state_dir,
            ..Default::default()
        }
    }
}

/// Read-only view the index exposes to framework providers.
pub trait SymbolLookup: Send + Sync {
    fn methods_named(&self, owner_fqn: &str, name: &str) -> Vec<Symbol>;
}

/// Extension point for framework navigation. Implementations never mutate the core index
/// directly; they return `Link`s that the index merges.
pub trait FrameworkProvider: Send + Sync {
    fn id(&self) -> &'static str;

    /// Cheap, local detection (file globs), never a build command.
    fn detect(&self, ctx: &ProjectContext) -> bool;

    /// Produce cross-file edges for the files discovered so far.
    fn contribute(
        &self,
        ctx: &ProjectContext,
        files: &[PathBuf],
        lookup: &dyn SymbolLookup,
    ) -> Vec<Link>;
}

/// Where a navigation request came from, used to route to providers on core misses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationKind {
    Definition,
    References,
    Implementation,
    Related,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_contains_is_end_exclusive() {
        let range = Range::new(
            Position {
                line: 1,
                character: 4,
            },
            Position {
                line: 1,
                character: 8,
            },
        );
        assert!(range.contains(Position {
            line: 1,
            character: 4
        }));
        assert!(range.contains(Position {
            line: 1,
            character: 7
        }));
        assert!(!range.contains(Position {
            line: 1,
            character: 8
        }));
    }

    #[test]
    fn symbol_id_is_stable_and_order_sensitive() {
        assert_eq!(symbol_id(&["a", "b"]), symbol_id(&["a", "b"]));
        assert_ne!(symbol_id(&["a", "b"]), symbol_id(&["b", "a"]));
    }
}
