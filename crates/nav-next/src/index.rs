//! Single-writer index construction and read-only query execution.

use crate::model::{
    FileContribution, FileId, Occurrence, OccurrenceRole, Position, Span, Symbol, SymbolId,
    SymbolKind, TypeEdge, UnresolvedOccurrence,
};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// A jump target in workspace coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub file_id: FileId,
    pub span: Span,
}

/// The immutable result of one successful index build.
///
/// All query methods are read-only. A query must never mutate this snapshot or
/// load additional state; shard loading belongs to a later, explicit cache
/// layer outside the snapshot contract.
#[derive(Debug, Clone, Default)]
pub struct IndexSnapshot {
    generation: u64,
    files: HashSet<FileId>,
    symbols: BTreeMap<SymbolId, Symbol>,
    by_name: HashMap<String, Vec<SymbolId>>,
    by_fqn: HashMap<String, Vec<SymbolId>>,
    children_by_parent: HashMap<SymbolId, Vec<SymbolId>>,
    parents_by_child: HashMap<SymbolId, Vec<SymbolId>>,
    symbols_by_file: HashMap<FileId, Vec<SymbolId>>,
    occurrences_by_symbol: HashMap<SymbolId, Vec<Occurrence>>,
    occurrences_by_file: HashMap<FileId, Vec<Occurrence>>,
}

impl IndexSnapshot {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    pub fn occurrence_count(&self) -> usize {
        self.occurrences_by_symbol.values().map(Vec::len).sum()
    }

    pub fn symbol(&self, id: SymbolId) -> Option<&Symbol> {
        self.symbols.get(&id)
    }

    pub fn symbols_named(&self, name: &str) -> Vec<Symbol> {
        self.by_name
            .get(name)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.symbols.get(id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn symbol_by_fqn(&self, fqn: &str) -> Vec<Symbol> {
        self.by_fqn
            .get(fqn)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.symbols.get(id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns every symbol declared in one file.
    pub fn symbols_in_file(&self, file: FileId) -> Vec<Symbol> {
        self.symbols_by_file
            .get(&file)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.symbols.get(id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn occurrences_of(&self, id: SymbolId) -> Vec<Occurrence> {
        self.occurrences_by_symbol
            .get(&id)
            .cloned()
            .unwrap_or_default()
    }

    fn descendants(&self, root: SymbolId) -> Vec<SymbolId> {
        let mut visited = HashSet::new();
        let mut queued = HashSet::new();
        let mut result = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(root);
        queued.insert(root);
        while let Some(parent) = queue.pop_front() {
            if !visited.insert(parent) {
                continue;
            }
            if let Some(children) = self.children_by_parent.get(&parent) {
                for child in children {
                    if *child != root && queued.insert(*child) {
                        result.push(*child);
                        queue.push_back(*child);
                    }
                }
            }
        }
        result
    }

    fn ancestors(&self, root: SymbolId) -> Vec<SymbolId> {
        let mut visited = HashSet::new();
        let mut queued = HashSet::new();
        let mut result = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(root);
        queued.insert(root);
        while let Some(child) = queue.pop_front() {
            if !visited.insert(child) {
                continue;
            }
            if let Some(parents) = self.parents_by_child.get(&child) {
                for parent in parents {
                    if *parent != root && queued.insert(*parent) {
                        result.push(*parent);
                        queue.push_back(*parent);
                    }
                }
            }
        }
        result
    }

    fn owner_of(&self, symbol: &Symbol) -> Option<SymbolId> {
        symbol.owner.or(if symbol.kind.is_type() {
            Some(symbol.id)
        } else {
            None
        })
    }

    fn methods_in_types(&self, types: &[SymbolId], name: &str) -> Vec<Symbol> {
        let mut out = Vec::new();
        for type_id in types {
            if self.symbols.contains_key(type_id) {
                for symbol in self.symbols.values() {
                    if symbol.kind.is_method()
                        && symbol.name == name
                        && symbol.owner == Some(*type_id)
                    {
                        out.push(symbol.clone());
                    }
                }
            }
        }
        out
    }
}

/// The single writer that turns per-file contributions into an immutable snapshot.
#[derive(Debug, Default)]
pub struct IndexBuilder {
    contributions: BTreeMap<FileId, FileContribution>,
    generation: u64,
}

impl IndexBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one parsed file. Contributions may reference symbols from files that
    /// have not been added yet; validation happens in `build`.
    pub fn add_file(&mut self, contribution: FileContribution) -> Result<(), String> {
        if self.contributions.contains_key(&contribution.file_id) {
            return Err(format!(
                "duplicate contribution for file {:?}",
                contribution.file_id
            ));
        }
        if contribution
            .symbols
            .iter()
            .any(|symbol| symbol.file_id != contribution.file_id)
        {
            return Err(format!(
                "symbol file_id does not match contribution {:?}",
                contribution.file_id
            ));
        }
        self.contributions
            .insert(contribution.file_id, contribution);
        Ok(())
    }

    /// Replaces the contribution for one file.
    ///
    /// This is the single-file update path used by editor events. It does not
    /// rebuild the snapshot; callers still invoke `build` afterwards.
    pub fn replace_file(&mut self, contribution: FileContribution) -> Result<(), String> {
        if contribution
            .symbols
            .iter()
            .any(|symbol| symbol.file_id != contribution.file_id)
        {
            return Err(format!(
                "symbol file_id does not match contribution {:?}",
                contribution.file_id
            ));
        }
        self.contributions
            .insert(contribution.file_id, contribution);
        Ok(())
    }

    /// Removes one file contribution.
    pub fn remove_file(&mut self, file: FileId) -> Result<(), String> {
        if self.contributions.remove(&file).is_none() {
            return Err(format!("missing contribution for file {:?}", file));
        }
        Ok(())
    }

    /// Returns every file currently known to the builder.
    pub fn file_ids(&self) -> impl Iterator<Item = FileId> + '_ {
        self.contributions.keys().copied()
    }

    /// Builds the next immutable snapshot.
    pub fn build(&mut self) -> Result<Arc<IndexSnapshot>, String> {
        self.generation += 1;
        let generation = self.generation;

        let mut snapshot = IndexSnapshot {
            generation,
            ..Default::default()
        };
        let mut seen_symbol_ids = HashSet::new();

        for contribution in self.contributions.values() {
            snapshot.files.insert(contribution.file_id);
            for symbol in &contribution.symbols {
                if !seen_symbol_ids.insert(symbol.id) {
                    return Err(format!("duplicate symbol id {:?}", symbol.id));
                }
                snapshot.symbols.insert(symbol.id, symbol.clone());
                snapshot
                    .symbols_by_file
                    .entry(contribution.file_id)
                    .or_default()
                    .push(symbol.id);
                snapshot
                    .by_name
                    .entry(symbol.name.clone())
                    .or_default()
                    .push(symbol.id);
                snapshot
                    .by_fqn
                    .entry(symbol.fqn.clone())
                    .or_default()
                    .push(symbol.id);
            }
        }

        let mut resolved_type_edges = Vec::new();
        for contribution in self.contributions.values() {
            for unresolved in &contribution.unresolved_type_edges {
                if !snapshot.symbols.contains_key(&unresolved.child) {
                    return Err(format!(
                        "unresolved type edge references missing child {:?}",
                        unresolved.child
                    ));
                }
                let parent_id = snapshot
                    .by_name
                    .get(&unresolved.parent_name)
                    .and_then(|ids| {
                        ids.iter().find_map(|id| {
                            snapshot
                                .symbols
                                .get(id)
                                .filter(|symbol| symbol.kind.is_type())
                                .map(|_| *id)
                        })
                    });
                if let Some(parent) = parent_id {
                    resolved_type_edges.push(TypeEdge {
                        child: unresolved.child,
                        parent,
                        kind: unresolved.kind,
                    });
                }
            }
        }

        let mut all_type_edges = resolved_type_edges;
        for contribution in self.contributions.values() {
            all_type_edges.extend(contribution.type_edges.iter().cloned());
        }
        for edge in all_type_edges {
            if !snapshot.symbols.contains_key(&edge.child)
                || !snapshot.symbols.contains_key(&edge.parent)
            {
                return Err(format!(
                    "type edge references missing symbol child={:?} parent={:?}",
                    edge.child, edge.parent
                ));
            }
            snapshot
                .children_by_parent
                .entry(edge.parent)
                .or_default()
                .push(edge.child);
            snapshot
                .parents_by_child
                .entry(edge.child)
                .or_default()
                .push(edge.parent);
        }

        for contribution in self.contributions.values() {
            for occurrence in &contribution.occurrences {
                if !snapshot.symbols.contains_key(&occurrence.symbol_id) {
                    return Err(format!(
                        "occurrence references missing symbol {:?}",
                        occurrence.symbol_id
                    ));
                }
                snapshot
                    .occurrences_by_symbol
                    .entry(occurrence.symbol_id)
                    .or_default()
                    .push(occurrence.clone());
                snapshot
                    .occurrences_by_file
                    .entry(occurrence.file_id)
                    .or_default()
                    .push(occurrence.clone());
            }
        }

        for contribution in self.contributions.values() {
            for unresolved in &contribution.unresolved {
                if let Some(symbol_id) = resolve_unresolved(&snapshot, unresolved) {
                    let occurrence = Occurrence {
                        symbol_id,
                        file_id: unresolved.file_id,
                        span: unresolved.span,
                        role: unresolved.role,
                    };
                    snapshot
                        .occurrences_by_symbol
                        .entry(symbol_id)
                        .or_default()
                        .push(occurrence.clone());
                    snapshot
                        .occurrences_by_file
                        .entry(unresolved.file_id)
                        .or_default()
                        .push(occurrence);
                }
            }
        }

        Ok(Arc::new(snapshot))
    }
}

/// Read-only query facade over one immutable snapshot.
#[derive(Debug, Clone)]
pub struct QueryEngine {
    snapshot: Arc<IndexSnapshot>,
}

impl QueryEngine {
    pub fn new(snapshot: Arc<IndexSnapshot>) -> Self {
        Self { snapshot }
    }

    pub fn snapshot(&self) -> &IndexSnapshot {
        &self.snapshot
    }

    pub fn symbol_at(&self, file: FileId, position: Position) -> Option<SymbolId> {
        let mut best: Option<(usize, u8, SymbolId)> = None;
        for occurrence in self
            .snapshot
            .occurrences_by_file
            .get(&file)
            .into_iter()
            .flatten()
        {
            if occurrence.span.contains(position) {
                let width = occurrence.span.width();
                let role_rank = if occurrence.role == OccurrenceRole::Definition {
                    0
                } else {
                    1
                };
                let rank = (width, role_rank, occurrence.symbol_id);
                if best.map(|current| rank < current).unwrap_or(true) {
                    best = Some(rank);
                }
            }
        }
        if let Some((_, _, symbol_id)) = best {
            return Some(symbol_id);
        }

        self.snapshot
            .symbols_by_file
            .get(&file)
            .into_iter()
            .flatten()
            .find_map(|symbol_id| {
                let symbol = self.snapshot.symbol(*symbol_id)?;
                symbol
                    .selection_span
                    .contains(position)
                    .then_some(*symbol_id)
            })
    }

    pub fn definition_at(&self, file: FileId, position: Position) -> Vec<Location> {
        self.symbol_at(file, position)
            .map(|symbol_id| self.definition(symbol_id))
            .unwrap_or_default()
    }

    pub fn implementations_at(&self, file: FileId, position: Position) -> Vec<Symbol> {
        self.symbol_at(file, position)
            .map(|symbol_id| self.implementations(symbol_id))
            .unwrap_or_default()
    }

    pub fn supertypes_at(&self, file: FileId, position: Position) -> Vec<Symbol> {
        self.symbol_at(file, position)
            .map(|symbol_id| self.supertypes(symbol_id))
            .unwrap_or_default()
    }

    pub fn definition(&self, id: SymbolId) -> Vec<Location> {
        self.snapshot
            .symbol(id)
            .map(|symbol| {
                vec![Location {
                    file_id: symbol.file_id,
                    span: symbol.selection_span,
                }]
            })
            .unwrap_or_default()
    }

    pub fn implementations(&self, id: SymbolId) -> Vec<Symbol> {
        let Some(symbol) = self.snapshot.symbol(id) else {
            return Vec::new();
        };
        if symbol.kind.is_type() {
            return self
                .snapshot
                .descendants(id)
                .into_iter()
                .filter_map(|child| self.snapshot.symbol(child))
                .filter(|child| child.kind.is_type())
                .cloned()
                .collect();
        }
        if !symbol.kind.is_method() {
            return Vec::new();
        }
        let Some(owner) = self.snapshot.owner_of(symbol) else {
            return Vec::new();
        };
        let implementors = self.snapshot.descendants(owner);
        self.snapshot.methods_in_types(&implementors, &symbol.name)
    }

    pub fn supertypes(&self, id: SymbolId) -> Vec<Symbol> {
        let Some(symbol) = self.snapshot.symbol(id) else {
            return Vec::new();
        };
        if symbol.kind.is_type() {
            return self
                .snapshot
                .ancestors(id)
                .into_iter()
                .filter_map(|parent| self.snapshot.symbol(parent))
                .filter(|parent| parent.kind.is_type())
                .cloned()
                .collect();
        }
        if !symbol.kind.is_method() {
            return Vec::new();
        }
        let Some(owner) = self.snapshot.owner_of(symbol) else {
            return Vec::new();
        };
        let supertypes = self.snapshot.ancestors(owner);
        self.snapshot.methods_in_types(&supertypes, &symbol.name)
    }

    pub fn references(&self, id: SymbolId) -> Vec<Occurrence> {
        let mut occurrences = self.snapshot.occurrences_of(id);
        if let Some(symbol) = self.snapshot.symbol(id) {
            if symbol.kind.is_method() {
                for implementation in self.implementations(id) {
                    occurrences.extend(self.snapshot.occurrences_of(implementation.id));
                }
            }
        }
        occurrences.sort_by_key(|occurrence| {
            (
                occurrence.file_id,
                occurrence.span.start.line,
                occurrence.span.start.character,
            )
        });
        occurrences
    }

    pub fn search_methods(&self, query: &str, limit: usize) -> Vec<Symbol> {
        let query = query
            .split('(')
            .next()
            .unwrap_or(query)
            .trim()
            .replace('#', ".")
            .to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut matches: Vec<(u8, Symbol)> = self
            .snapshot
            .symbols
            .values()
            .filter(|symbol| symbol.kind.is_method())
            .filter_map(|symbol| {
                let full = symbol
                    .owner
                    .and_then(|owner| self.snapshot.symbol(owner))
                    .map(|owner| format!("{}.{}", owner.fqn, symbol.name))
                    .unwrap_or_else(|| symbol.name.clone())
                    .to_lowercase();
                let name = symbol.name.to_lowercase();
                let rank = if full == query {
                    0
                } else if full.ends_with(&format!(".{query}")) {
                    1
                } else if name == query {
                    2
                } else if full.contains(&query) {
                    3
                } else if name.contains(&query) {
                    4
                } else {
                    return None;
                };
                Some((rank, symbol.clone()))
            })
            .collect();
        matches.sort_by_key(|(rank, symbol)| {
            (
                *rank,
                symbol.fqn.len(),
                symbol.fqn.clone(),
                symbol.name.clone(),
            )
        });
        matches.truncate(limit);
        matches.into_iter().map(|(_, symbol)| symbol).collect()
    }
}

fn resolve_unresolved(
    snapshot: &IndexSnapshot,
    unresolved: &UnresolvedOccurrence,
) -> Option<SymbolId> {
    let ids = snapshot.by_name.get(&unresolved.name)?;
    let mut candidates: Vec<&Symbol> = ids
        .iter()
        .filter_map(|id| snapshot.symbols.get(id))
        .filter(|symbol| {
            unresolved
                .kind_hint
                .map(|hint| kind_matches(symbol.kind, hint))
                .unwrap_or(true)
        })
        .collect();
    if candidates.is_empty() {
        return None;
    }
    if let Some(receiver) = &unresolved.receiver_type {
        let simple = simple_type_name(receiver);
        let narrowed: Vec<&Symbol> = candidates
            .iter()
            .copied()
            .filter(|symbol| {
                symbol
                    .owner
                    .and_then(|owner| snapshot.symbols.get(&owner))
                    .map(|owner| simple_type_name(&owner.fqn) == simple)
                    .unwrap_or(false)
            })
            .collect();
        if !narrowed.is_empty() {
            candidates = narrowed;
        }
    }
    if let Some(expected) = unresolved.argument_count {
        let narrowed: Vec<&Symbol> = candidates
            .iter()
            .copied()
            .filter(|symbol| descriptor_argument_count(&symbol.descriptor) == Some(expected))
            .collect();
        if !narrowed.is_empty() {
            candidates = narrowed;
        }
    }
    candidates.sort_by_key(|symbol| (symbol.synthetic as u8, symbol.fqn.len()));
    candidates.first().map(|symbol| symbol.id)
}

fn kind_matches(actual: SymbolKind, hint: SymbolKind) -> bool {
    match hint {
        SymbolKind::Method => actual.is_method(),
        SymbolKind::Field => matches!(actual, SymbolKind::Field | SymbolKind::Synthetic),
        SymbolKind::Class => actual.is_type(),
        _ => true,
    }
}

fn simple_type_name(fqn: &str) -> &str {
    fqn.rsplit('.').next().unwrap_or(fqn)
}

fn descriptor_argument_count(descriptor: &str) -> Option<usize> {
    let open = descriptor.find('(')?;
    let close = descriptor.rfind(')')?;
    if close < open {
        return None;
    }
    let inside = &descriptor[open + 1..close];
    if inside.trim().is_empty() {
        Some(0)
    } else {
        Some(inside.split(',').count())
    }
}
