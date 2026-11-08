//! Workspace-wide navigation index.
//!
//! The index is the single source of truth for every LSP request. It is built by
//! parsing Java sources with `nav-java`, binding cross-file references, then
//! merging framework `Link`s. State is persisted only under `<workspace>/.ld-java-analyze`.
//! No Maven process is started and no network access is performed.

use nav_core::*;
use nav_java::{JavaParser, MybatisProvider};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Bumped whenever the parser's contribution shape changes; old snapshots are
/// then discarded instead of being reused with stale bindings.
pub const INDEX_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    root: PathBuf,
    state_dir: PathBuf,
    modules: Vec<PathBuf>,
    classpath: Vec<PathBuf>,
    source_jars: Vec<PathBuf>,
    files: HashMap<String, FileContribution>,
    stamps: HashMap<String, u64>,
    reference_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub files: usize,
    pub changed: usize,
    pub symbols: usize,
    pub occurrences: usize,
    pub links: usize,
    /// Files whose Java source could not be parsed, e.g. `uri: error`.
    pub failed_files: Vec<String>,
}

/// All derived maps are rebuilt from `files`; none of them need to be persisted.
#[derive(Default)]
pub struct WorkspaceIndex {
    ctx: ProjectContext,
    all_files: Vec<PathBuf>,
    files: HashMap<String, FileContribution>,
    stamps: HashMap<String, u64>,
    symbols: HashMap<SymbolId, Symbol>,
    by_name: HashMap<String, Vec<SymbolId>>,
    by_fqn: HashMap<String, Vec<SymbolId>>,
    occurrences: HashMap<SymbolId, Vec<Occurrence>>,
    /// Inverted by document URI so `symbol_at` never scans the whole workspace.
    occurrences_by_uri: HashMap<String, Vec<Occurrence>>,
    /// Occurrences produced by binding cross-file `UnresolvedOccurrence`s.
    resolved: Vec<Occurrence>,
    links: Vec<Link>,
    type_edges: Vec<TypeEdge>,
    reference_roots: Vec<PathBuf>,
    /// Caller-provided configuration; never read from a stale snapshot.
    scan_config: ScanConfig,
    /// Effective rules = configuration + optional `.ld-java-analyze/ignore`.
    scan_rules: ScanRules,
    /// URIs whose truth currently comes from an open editor, never persisted.
    dirty: HashSet<String>,
}

impl WorkspaceIndex {
    /// Opens (or reuses) the on-disk snapshot for `root`. `reference_roots` are
    /// relative to `root` unless absolute; an empty list means "whole workspace".
    pub fn open(root: impl AsRef<Path>, reference_roots: &[String]) -> Self {
        let config = ScanConfig {
            reference_roots: reference_roots.to_vec(),
            ..Default::default()
        };
        Self::open_with_config(root, &config)
    }

    /// Full configuration entry point: reference roots plus scan exclusion rules.
    pub fn open_with_config(root: impl AsRef<Path>, config: &ScanConfig) -> Self {
        let mut ctx = ProjectContext::new(root);
        let rules = ScanRules::combined(&config.exclude_rules, None, config.use_builtin_excludes);
        ctx.scan_rules = rules.clone();
        nav_maven::discover(&mut ctx);
        let roots = normalize_roots(&ctx.root, &config.reference_roots);
        let mut index = Self {
            ctx,
            all_files: Vec::new(),
            files: HashMap::new(),
            stamps: HashMap::new(),
            symbols: HashMap::new(),
            by_name: HashMap::new(),
            by_fqn: HashMap::new(),
            occurrences: HashMap::new(),
            occurrences_by_uri: HashMap::new(),
            resolved: Vec::new(),
            links: Vec::new(),
            type_edges: Vec::new(),
            reference_roots: roots.clone(),
            scan_config: config.clone(),
            scan_rules: rules,
            dirty: HashSet::new(),
        };
        if let Some(snapshot) = index.load() {
            if snapshot.version == INDEX_VERSION && snapshot.root == index.ctx.root {
                index.apply(snapshot);
            }
        }
        // Caller configuration wins over a stale snapshot.
        index.reference_roots = roots;
        index
    }

    pub fn root(&self) -> &Path {
        &self.ctx.root
    }

    pub fn state_dir(&self) -> &Path {
        &self.ctx.state_dir
    }

    pub fn context(&self) -> &ProjectContext {
        &self.ctx
    }

    pub fn reference_roots(&self) -> Vec<String> {
        self.reference_roots
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    pub fn set_reference_roots(&mut self, roots: &[String]) {
        self.reference_roots = normalize_roots(&self.ctx.root, roots);
        self.scan_config.reference_roots = roots.to_vec();
    }

    pub fn scan_config(&self) -> &ScanConfig {
        &self.scan_config
    }

    pub fn scan_rules(&self) -> &ScanRules {
        &self.scan_rules
    }

    /// Replaces the whole client configuration and immediately rescans.
    pub fn set_scan_config(&mut self, config: &ScanConfig) -> ScanReport {
        self.scan_config = config.clone();
        self.reference_roots = normalize_roots(&self.ctx.root, &config.reference_roots);
        self.scan()
    }

    /// Replaces just the exclusion rules and immediately rescans.
    pub fn set_exclude_rules(&mut self, lines: &[String]) -> ScanReport {
        self.scan_config.exclude_rules = lines.to_vec();
        self.scan()
    }

    /// Re-reads `.ld-java-analyze/ignore` on every scan so editing that file takes effect
    /// on the next reindex without restarting the server.
    fn refresh_rules(&mut self) {
        let file_text = fs::read_to_string(self.ctx.state_dir.join("ignore")).ok();
        self.scan_rules = ScanRules::combined(
            &self.scan_config.exclude_rules,
            file_text.as_deref(),
            self.scan_config.use_builtin_excludes,
        );
        self.ctx.scan_rules = self.scan_rules.clone();
    }

    /// Incremental workspace scan: only Java files whose stamp changed are reparsed.
    pub fn scan(&mut self) -> ScanReport {
        self.refresh_rules();
        nav_maven::discover(&mut self.ctx);
        let all = discover_files(&self.ctx.root, &self.scan_rules);
        self.all_files = all.clone();

        let java_files: Vec<PathBuf> = all
            .iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "java"))
            .cloned()
            .collect();

        let mut next_stamps: HashMap<String, u64> = HashMap::new();
        let mut changed: Vec<(String, String)> = Vec::new();
        for path in &java_files {
            let uri = file_uri(path);
            let stamp = stamp_of(path);
            next_stamps.insert(uri.clone(), stamp);
            // An open editor owns this file; do not overwrite it from disk.
            if self.dirty.contains(&uri) {
                continue;
            }
            if self.stamps.get(&uri).copied().unwrap_or_default() != stamp
                || !self.files.contains_key(&uri)
            {
                if let Ok(text) = fs::read_to_string(path) {
                    changed.push((uri, text));
                }
            }
        }

        let live: HashSet<String> = next_stamps.keys().cloned().collect();
        self.files.retain(|uri, _| live.contains(uri));
        self.stamps = next_stamps;

        let parsed: Vec<(String, Result<FileContribution, String>)> = changed
            .par_iter()
            .map(|(uri, text)| (uri.clone(), JavaParser::new().parse_file(uri, text)))
            .collect();
        let mut changed_count = 0usize;
        let mut failed_files = Vec::new();
        for (uri, result) in parsed {
            match result {
                Ok(contribution) => {
                    self.files.insert(uri, contribution);
                    changed_count += 1;
                }
                Err(error) => failed_files.push(format!("{uri}: {error}")),
            }
        }

        self.rebuild();
        ScanReport {
            files: self.files.len(),
            changed: changed_count,
            symbols: self.symbols.len(),
            occurrences: self.occurrences.values().map(Vec::len).sum(),
            links: self.links.len(),
            failed_files,
        }
    }

    /// Whether a URI is inside an excluded directory or has an excluded file name.
    fn uri_excluded(&self, uri: &str) -> bool {
        let normalized = uri.replace('\\', "/");
        let root = file_uri(&self.ctx.root);
        let prefix = root.trim_end_matches('/');
        let relative = normalized.strip_prefix(prefix).unwrap_or(&normalized);
        let parts: Vec<&str> = relative
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        if let Some(name) = parts.last() {
            if self.scan_rules.is_excluded_file(name) {
                return true;
            }
        }
        parts[..parts.len().saturating_sub(1)]
            .iter()
            .any(|part| self.scan_rules.is_excluded_dir(part))
    }

    /// Replaces one file's contribution from an in-memory document (didOpen/didChange).
    pub fn update_document(&mut self, uri: &str, text: &str) -> Result<(), String> {
        if self.uri_excluded(uri) {
            self.files.remove(uri);
            self.dirty.remove(uri);
            self.rebuild();
            return Ok(());
        }
        let contribution = JavaParser::new().parse_file(uri, text)?;
        self.files.insert(uri.to_string(), contribution);
        self.dirty.insert(uri.to_string());
        self.rebuild();
        Ok(())
    }

    /// Drops the in-memory override and re-reads the file from disk.
    pub fn reload_from_disk(&mut self, uri: &str) {
        let path = uri_to_path(uri);
        if self.uri_excluded(uri) {
            self.files.remove(uri);
            self.dirty.remove(uri);
            self.rebuild();
            return;
        }
        match fs::read_to_string(&path) {
            Ok(text) => {
                if let Ok(contribution) = JavaParser::new().parse_file(uri, &text) {
                    self.files.insert(uri.to_string(), contribution);
                }
            }
            Err(_) => {
                self.files.remove(uri);
            }
        }
        self.dirty.remove(uri);
        self.stamps.insert(uri.to_string(), stamp_of(&path));
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.symbols.clear();
        self.by_name.clear();
        self.by_fqn.clear();
        self.occurrences.clear();
        self.occurrences_by_uri.clear();
        self.type_edges.clear();

        for contribution in self.files.values() {
            for symbol in &contribution.symbols {
                self.symbols.insert(symbol.id, symbol.clone());
                self.by_name
                    .entry(symbol.name.clone())
                    .or_default()
                    .push(symbol.id);
                self.by_fqn
                    .entry(symbol.fqn.clone())
                    .or_default()
                    .push(symbol.id);
            }
            for occurrence in &contribution.occurrences {
                self.occurrences
                    .entry(occurrence.symbol_id)
                    .or_default()
                    .push(occurrence.clone());
                self.occurrences_by_uri
                    .entry(occurrence.uri.clone())
                    .or_default()
                    .push(occurrence.clone());
            }
            self.type_edges
                .extend(contribution.type_edges.iter().cloned());
        }

        let unresolved: Vec<UnresolvedOccurrence> = self
            .files
            .values()
            .flat_map(|contribution| contribution.unresolved.iter().cloned())
            .collect();
        let mut resolved = Vec::new();
        for item in unresolved {
            if let Some(symbol_id) = self.resolve_unresolved(&item) {
                resolved.push(Occurrence {
                    symbol_id,
                    uri: item.uri,
                    range: item.range,
                    role: item.role,
                });
            }
        }
        self.resolved = resolved.clone();
        for occurrence in resolved {
            self.occurrences
                .entry(occurrence.symbol_id)
                .or_default()
                .push(occurrence.clone());
            self.occurrences_by_uri
                .entry(occurrence.uri.clone())
                .or_default()
                .push(occurrence);
        }

        let ctx = self.ctx.clone();
        let files = self.all_files.clone();
        self.links = MybatisProvider.contribute(&ctx, &files, self);
    }

    fn resolve_unresolved(&self, item: &UnresolvedOccurrence) -> Option<SymbolId> {
        let ids = self.by_name.get(&item.name)?;
        let mut candidates: Vec<&Symbol> = ids
            .iter()
            .filter_map(|id| self.symbols.get(id))
            .filter(|symbol| {
                item.kind_hint
                    .map(|hint| kind_matches(symbol.kind, hint))
                    .unwrap_or(true)
            })
            // Locals and parameters only ever bind inside their own file.
            .filter(|symbol| !matches!(symbol.kind, SymbolKind::Local | SymbolKind::Parameter))
            // An unqualified read/write must not accidentally bind to a type or method.
            .filter(|symbol| {
                item.kind_hint.is_some()
                    || !matches!(
                        item.role,
                        OccurrenceRole::Read | OccurrenceRole::Write | OccurrenceRole::Reference
                    )
                    || matches!(symbol.kind, SymbolKind::Field | SymbolKind::Synthetic)
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        if let Some(receiver) = &item.receiver_type {
            let simple = simple_type_name(receiver);
            let narrowed: Vec<&Symbol> = candidates
                .iter()
                .copied()
                .filter(|symbol| {
                    symbol.owner_fqn.as_deref().map(simple_type_name).as_deref()
                        == Some(simple.as_str())
                })
                .collect();
            if !narrowed.is_empty() {
                candidates = narrowed;
            }
        }
        // Overloads with a different arity cannot be the target of this call.
        if let Some(expected) = item.argument_count {
            let narrowed: Vec<&Symbol> = candidates
                .iter()
                .copied()
                .filter(|symbol| descriptor_parameter_count(&symbol.descriptor) == Some(expected))
                .collect();
            if !narrowed.is_empty() {
                candidates = narrowed;
            }
        }
        candidates.sort_by_key(|symbol| (symbol.synthetic as u8, symbol.fqn.len()));
        candidates.first().map(|symbol| symbol.id)
    }

    // ------------------------------------------------------------- queries

    pub fn symbol(&self, id: SymbolId) -> Option<&Symbol> {
        self.symbols.get(&id)
    }

    pub fn all_symbols(&self) -> Vec<Symbol> {
        self.symbols.values().cloned().collect()
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

    /// Symbol at a position: prefer occurrences (references), then declarations
    /// whose name range covers the position (covers Lombok synthetic symbols).
    pub fn symbol_at(&self, uri: &str, position: Position) -> Option<SymbolId> {
        let mut best: Option<(usize, u8, SymbolId)> = None;
        for occurrence in self.occurrences_by_uri.get(uri).into_iter().flatten() {
            if occurrence.range.contains(position) {
                let width = range_width(occurrence.range);
                let role_rank = if occurrence.role == OccurrenceRole::Definition {
                    0u8
                } else {
                    1u8
                };
                let rank = (width, role_rank, occurrence.symbol_id);
                if best.map(|current| rank < current).unwrap_or(true) {
                    best = Some(rank);
                }
            }
        }
        if let Some((_, _, id)) = best {
            return Some(id);
        }
        self.files
            .get(uri)?
            .symbols
            .iter()
            .find(|symbol| symbol.selection_range.contains(position))
            .map(|symbol| symbol.id)
    }

    pub fn definition_targets(&self, id: SymbolId) -> Vec<LinkEndpoint> {
        self.symbols
            .get(&id)
            .map(|symbol| {
                vec![LinkEndpoint {
                    uri: symbol.uri.clone(),
                    range: symbol.selection_range,
                    label: symbol.display_name(),
                }]
            })
            .unwrap_or_default()
    }

    /// Framework links touching this position (MyBatis XML <-> Mapper method).
    pub fn link_targets(&self, uri: &str, position: Position) -> Vec<LinkEndpoint> {
        nav_java::mybatis::targets_at(&self.links, uri, position)
    }

    pub fn links(&self) -> &[Link] {
        &self.links
    }

    /// `roots == None` uses the configured `referenceRoots`; `Some([])` means all.
    pub fn references(
        &self,
        id: SymbolId,
        roots: Option<&[String]>,
        include_declaration: bool,
    ) -> Vec<Occurrence> {
        let effective: Vec<PathBuf> = match roots {
            Some(values) => normalize_roots(&self.ctx.root, values),
            None => self.reference_roots.clone(),
        };
        // IDEA-style "find usages" on an interface method also lists overriding
        // methods in implementing classes.
        let mut ids = vec![id];
        if let Some(symbol) = self.symbols.get(&id) {
            if matches!(symbol.kind, SymbolKind::Method | SymbolKind::Synthetic) {
                ids.extend(
                    self.implementations(id)
                        .into_iter()
                        .filter(|candidate| {
                            matches!(
                                candidate.kind,
                                SymbolKind::Method
                                    | SymbolKind::Constructor
                                    | SymbolKind::Synthetic
                            )
                        })
                        .map(|candidate| candidate.id),
                );
            }
        }
        let mut out: Vec<Occurrence> = Vec::new();
        for symbol_id in ids {
            let Some(items) = self.occurrences.get(&symbol_id) else {
                continue;
            };
            out.extend(
                items
                    .iter()
                    .filter(|occurrence| {
                        (include_declaration || occurrence.role != OccurrenceRole::Definition)
                            && self.path_allowed(&occurrence.uri, &effective)
                    })
                    .cloned(),
            );
        }
        out.sort_by_key(|occurrence| {
            (
                occurrence.uri.clone(),
                occurrence.range.start.line,
                occurrence.range.start.character,
            )
        });
        let mut seen = HashSet::new();
        out.retain(|occurrence| seen.insert((occurrence.uri.clone(), occurrence.range)));
        out
    }

    pub fn highlights(&self, uri: &str, id: SymbolId) -> Vec<Occurrence> {
        self.occurrences
            .get(&id)
            .map(|items| {
                items
                    .iter()
                    .filter(|occurrence| occurrence.uri == uri)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn symbols_in_file(&self, uri: &str) -> Vec<Symbol> {
        let mut symbols: Vec<Symbol> = self
            .symbols
            .values()
            .filter(|symbol| symbol.uri == uri)
            .cloned()
            .collect();
        symbols.sort_by_key(|symbol| (symbol.range.start.line, symbol.range.start.character));
        symbols
    }

    pub fn workspace_symbols(&self, query: &str, limit: usize) -> Vec<Symbol> {
        let needle = query.to_lowercase();
        let mut matches: Vec<Symbol> = self
            .symbols
            .values()
            .filter(|symbol| {
                needle.is_empty()
                    || symbol.name.to_lowercase().contains(&needle)
                    || symbol.fqn.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        matches.sort_by(|a, b| a.name.cmp(&b.name).then(a.fqn.cmp(&b.fqn)));
        matches.truncate(limit);
        matches
    }

    /// IDEA-style "go to implementation(s)" using the collected type edges.
    pub fn implementations(&self, id: SymbolId) -> Vec<Symbol> {
        let Some(symbol) = self.symbols.get(&id) else {
            return Vec::new();
        };
        let owner = symbol
            .owner_fqn
            .clone()
            .unwrap_or_else(|| symbol.fqn.clone());
        let target = simple_type_name(&owner);
        let mut implementors = HashSet::new();
        let mut pending = vec![target.to_string()];
        let mut visited = HashSet::new();
        while let Some(parent) = pending.pop() {
            if !visited.insert(parent.clone()) {
                continue;
            }
            for edge in self
                .type_edges
                .iter()
                .filter(|edge| simple_type_name(&edge.parent_fqn) == parent)
            {
                if edge.child_fqn != owner && implementors.insert(edge.child_fqn.clone()) {
                    pending.push(simple_type_name(&edge.child_fqn).to_string());
                }
            }
        }

        let mut out = Vec::new();
        for candidate in self.symbols.values() {
            let is_type = matches!(
                candidate.kind,
                SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
            );
            let is_implementor = candidate
                .owner_fqn
                .as_deref()
                .map(|owner| implementors.contains(owner))
                .unwrap_or(false);
            let type_match = is_type && implementors.contains(&candidate.fqn);
            let method_match = !is_type && is_implementor && candidate.name == symbol.name;
            if type_match || method_match {
                out.push(candidate.clone());
            }
        }
        out.sort_by_key(|symbol| (symbol.uri.clone(), symbol.range.start.line));
        out
    }

    /// Type definition for a variable/field/parameter: resolves its declared type.
    pub fn type_definition(&self, id: SymbolId) -> Vec<Symbol> {
        let Some(symbol) = self.symbols.get(&id) else {
            return Vec::new();
        };
        if symbol.descriptor.is_empty() {
            return Vec::new();
        }
        let wanted = simple_type_name(&symbol.descriptor);
        self.symbols
            .values()
            .filter(|candidate| {
                matches!(
                    candidate.kind,
                    SymbolKind::Class
                        | SymbolKind::Interface
                        | SymbolKind::Enum
                        | SymbolKind::Annotation
                ) && candidate.name == wanted
            })
            .cloned()
            .collect()
    }

    pub fn bound_occurrences(&self, id: SymbolId) -> usize {
        self.occurrences.get(&id).map(Vec::len).unwrap_or(0)
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn occurrence_count(&self) -> usize {
        self.occurrences.values().map(Vec::len).sum()
    }

    // --------------------------------------------------------- persistence

    fn cache_path(&self) -> PathBuf {
        self.ctx.state_dir.join("cache").join("index.json")
    }

    /// Writes the snapshot into `<workspace>/.ld-java-analyze/cache`. Never writes elsewhere.
    pub fn save(&self) -> std::io::Result<PathBuf> {
        let snapshot = Snapshot {
            version: INDEX_VERSION,
            root: self.ctx.root.clone(),
            state_dir: self.ctx.state_dir.clone(),
            modules: self.ctx.modules.clone(),
            classpath: self.ctx.classpath.clone(),
            source_jars: self.ctx.source_jars.clone(),
            files: self
                .files
                .iter()
                .filter(|(uri, _)| !self.dirty.contains(*uri))
                .map(|(uri, contribution)| (uri.clone(), contribution.clone()))
                .collect(),
            stamps: self
                .stamps
                .iter()
                .filter(|(uri, _)| !self.dirty.contains(*uri))
                .map(|(uri, stamp)| (uri.clone(), *stamp))
                .collect(),
            reference_roots: self.reference_roots.clone(),
        };
        let path = self.cache_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.tmp");
        fs::write(
            &temp,
            serde_json::to_vec(&snapshot).map_err(std::io::Error::other)?,
        )?;
        fs::rename(temp, &path)?;
        Ok(path)
    }

    fn load(&self) -> Option<Snapshot> {
        let bytes = fs::read(self.cache_path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn apply(&mut self, snapshot: Snapshot) {
        self.ctx.modules = snapshot.modules;
        self.ctx.classpath = snapshot.classpath;
        self.ctx.source_jars = snapshot.source_jars;
        self.files = snapshot.files;
        self.stamps = snapshot.stamps;
        self.reference_roots = snapshot.reference_roots;
        self.ctx.scan_rules = self.scan_rules.clone();
        self.dirty.clear();
        self.all_files = discover_files(&self.ctx.root, &self.scan_rules);
        self.rebuild();
    }

    fn path_allowed(&self, uri: &str, roots: &[PathBuf]) -> bool {
        if roots.is_empty() {
            return true;
        }
        let path = uri_to_path(uri);
        roots.iter().any(|root| path_starts_with(&path, root))
    }
}

impl SymbolLookup for WorkspaceIndex {
    fn methods_named(&self, owner_fqn: &str, name: &str) -> Vec<Symbol> {
        self.by_name
            .get(name)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.symbols.get(id))
                    .filter(|symbol| {
                        matches!(
                            symbol.kind,
                            SymbolKind::Method | SymbolKind::Constructor | SymbolKind::Synthetic
                        )
                    })
                    .filter(|symbol| {
                        symbol.owner_fqn.as_deref() == Some(owner_fqn)
                            || symbol.fqn.starts_with(&format!("{owner_fqn}."))
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ------------------------------------------------------------------ helpers

fn discover_files(root: &Path, rules: &ScanRules) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| entry.path() == root || !nav_maven::is_ignored_by(entry, rules))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "java" || ext == "xml")
        })
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !rules.is_excluded_file(name))
        })
        .collect();
    files.sort();
    files
}

fn stamp_of(path: &Path) -> u64 {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => return 0,
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    modified ^ meta.len().rotate_left(17)
}

pub fn normalize_roots(root: &Path, roots: &[String]) -> Vec<PathBuf> {
    roots
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .collect()
}

/// True when a type with this simple name is defined by workspace sources.
/// Used to keep navigation inside the workspace instead of falling back to jars.
pub fn has_workspace_type(symbols: &[Symbol], simple: &str) -> bool {
    symbols.iter().any(|symbol| {
        symbol.name == simple
            && matches!(
                symbol.kind,
                SymbolKind::Class
                    | SymbolKind::Interface
                    | SymbolKind::Enum
                    | SymbolKind::Annotation
            )
    })
}

/// Counts top-level parameters in a method descriptor such as
/// `(List<T> items, long total)` — generics and nested annotations are ignored.
fn descriptor_parameter_count(descriptor: &str) -> Option<usize> {
    let open = descriptor.find('(')?;
    let close = descriptor.rfind(')')?;
    if close <= open {
        return None;
    }
    let inner = &descriptor[open + 1..close];
    if inner.trim().is_empty() {
        return Some(0);
    }
    let mut depth = 0i32;
    let mut count = 1usize;
    for character in inner.chars() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    Some(count)
}

fn simple_type_name(name: &str) -> String {
    let without_generics = name.split('<').next().unwrap_or(name);
    let without_array = without_generics
        .split('[')
        .next()
        .unwrap_or(without_generics);
    without_array
        .trim()
        .trim_end_matches("...")
        .rsplit('.')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn kind_matches(actual: SymbolKind, hint: SymbolKind) -> bool {
    match hint {
        SymbolKind::Method => matches!(actual, SymbolKind::Method | SymbolKind::Synthetic),
        SymbolKind::Field => matches!(actual, SymbolKind::Field | SymbolKind::Synthetic),
        SymbolKind::Class => matches!(
            actual,
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum | SymbolKind::Annotation
        ),
        _ => true,
    }
}

fn range_width(range: Range) -> usize {
    if range.start.line == range.end.line {
        (range.end.character - range.start.character) as usize
    } else {
        usize::MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempWorkspace(PathBuf);

    impl TempWorkspace {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "nav-index-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("src/demo")).unwrap();
            fs::create_dir_all(path.join("other")).unwrap();
            Self(path)
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, text).unwrap();
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const SERVICE: &str = "package demo;\npublic class UserService {\n    public String find(int id) { return \"x\"; }\n}\n";
    const CALLER: &str = "package demo;\npublic class Caller {\n    private UserService service;\n    public String run() { return service.find(1); }\n}\n";

    #[test]
    fn binds_cross_file_calls_and_filters_references_by_directory() {
        let workspace = TempWorkspace::new("refs");
        workspace.write("src/demo/UserService.java", SERVICE);
        workspace.write("src/demo/Caller.java", CALLER);
        workspace.write(
            "other/AnotherCaller.java",
            CALLER.replace("Caller", "AnotherCaller").as_str(),
        );

        let mut index = WorkspaceIndex::open(&workspace.0, &[]);
        let report = index.scan();
        assert!(report.files >= 3, "report={report:?}");
        assert!(report.links == 0);

        let caller_uri = file_uri(&workspace.0.join("src/demo/Caller.java"));
        let method = index
            .all_symbols()
            .into_iter()
            .find(|symbol| symbol.name == "find" && symbol.kind == SymbolKind::Method)
            .expect("find method");

        let all = index.references(method.id, None, true);
        assert!(all.len() >= 3, "expected cross-file calls, got {all:?}");

        let only_src = index.references(method.id, Some(&["src".to_string()]), true);
        assert!(only_src.iter().all(|occ| occ.uri.contains("/src/")));
        assert!(only_src.len() < all.len(), "filter did not narrow results");
        assert!(index
            .symbol_at(
                &caller_uri,
                Position {
                    line: 3,
                    character: 42
                }
            )
            .is_some());
    }

    #[test]
    fn interface_method_usages_include_overrides() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("testdata")
            .join("demo1");
        let mut index = WorkspaceIndex::open(&root, &[]);
        index.scan();
        let method = index
            .symbols_named("findById")
            .into_iter()
            .find(|symbol| symbol.owner_fqn.as_deref() == Some("demo.UserService"))
            .unwrap();
        let usages = index.references(method.id, None, true);
        assert!(usages
            .iter()
            .any(|occurrence| occurrence.uri.contains("UserServiceImpl")));
    }

    #[test]
    fn implementations_cross_maven_module_boundaries() {
        let workspace = TempWorkspace::new("cross-module-implementations");
        workspace.write(
            "api-module/src/main/java/demo/api/UserService.java",
            "package demo.api;\npublic interface UserService {\n    String findById(int id);\n}\n",
        );
        workspace.write(
            "service-module/src/main/java/demo/service/UserServiceImpl.java",
            "package demo.service;\nimport demo.api.UserService;\npublic class UserServiceImpl implements UserService {\n    public String findById(int id) { return \"x\"; }\n}\n",
        );

        let mut index = WorkspaceIndex::open(&workspace.0, &[]);
        let report = index.scan();
        assert_eq!(report.files, 2, "report={report:?}");

        let interface = index
            .symbols_named("UserService")
            .into_iter()
            .find(|symbol| symbol.fqn == "demo.api.UserService")
            .expect("interface in api module");
        let implementations = index.implementations(interface.id);
        assert!(
            implementations
                .iter()
                .any(|symbol| symbol.fqn == "demo.service.UserServiceImpl"),
            "implementations={implementations:?}"
        );

        let method = index
            .symbols_named("findById")
            .into_iter()
            .find(|symbol| symbol.owner_fqn.as_deref() == Some("demo.api.UserService"))
            .expect("interface method");
        let implementations = index.implementations(method.id);
        assert!(
            implementations.iter().any(|symbol| {
                symbol.owner_fqn.as_deref() == Some("demo.service.UserServiceImpl")
            }),
            "method implementations={implementations:?}"
        );
    }

    #[test]
    fn implementations_cross_modules_through_intermediate_interface() {
        let workspace = TempWorkspace::new("cross-module-transitive-implementations");
        workspace.write(
            "api-module/src/main/java/demo/api/UserService.java",
            "package demo.api;\npublic interface UserService {\n    String findById(int id);\n}\n",
        );
        workspace.write(
            "service-api-module/src/main/java/demo/service/InternalUserService.java",
            "package demo.service;\nimport demo.api.UserService;\npublic interface InternalUserService extends UserService {}\n",
        );
        workspace.write(
            "service-impl-module/src/main/java/demo/service/impl/UserServiceImpl.java",
            "package demo.service.impl;\nimport demo.service.InternalUserService;\npublic class UserServiceImpl implements InternalUserService {\n    public String findById(int id) { return \"x\"; }\n}\n",
        );

        let mut index = WorkspaceIndex::open(&workspace.0, &[]);
        assert_eq!(index.scan().files, 3);
        let interface = index
            .symbols_named("UserService")
            .into_iter()
            .find(|symbol| symbol.fqn == "demo.api.UserService")
            .expect("interface in api module");
        let implementations = index.implementations(interface.id);
        assert!(
            implementations
                .iter()
                .any(|symbol| symbol.fqn == "demo.service.impl.UserServiceImpl"),
            "transitive implementations={implementations:?}"
        );

        let method = index
            .symbols_named("findById")
            .into_iter()
            .find(|symbol| symbol.owner_fqn.as_deref() == Some("demo.api.UserService"))
            .expect("interface method");
        let usages = index.references(method.id, None, true);
        assert!(
            usages
                .iter()
                .any(|occurrence| occurrence.uri.contains("UserServiceImpl")),
            "cross-module override usages={usages:?}"
        );
    }

    #[test]
    fn open_document_override_is_not_persisted() {
        let workspace = TempWorkspace::new("dirty");
        workspace.write("src/demo/UserService.java", SERVICE);
        let mut index = WorkspaceIndex::open(&workspace.0, &[]);
        index.scan();
        let uri = file_uri(&workspace.0.join("src/demo/UserService.java"));
        index
            .update_document(&uri, "package demo; class Ghost {}")
            .unwrap();
        assert_eq!(index.symbols_named("Ghost").len(), 1);
        index.save().unwrap();

        let mut restored = WorkspaceIndex::open(&workspace.0, &[]);
        restored.scan();
        assert!(restored.symbols_named("Ghost").is_empty());
        assert_eq!(restored.symbols_named("UserService").len(), 1);
    }

    #[test]
    fn argument_count_picks_the_matching_overload() {
        let workspace = TempWorkspace::new("overloads");
        workspace.write(
            "src/demo/Api.java",
            "package demo;\npublic interface Api {\n    int list(int a);\n    int list(int a, int b);\n}\n",
        );
        let caller = "package demo;\npublic class Caller {\n    private Api api;\n    public int run() { return api.list(1); }\n}\n";
        workspace.write("src/demo/Caller.java", caller);

        let mut index = WorkspaceIndex::open(&workspace.0, &[]);
        index.scan();
        let uri = file_uri(&workspace.0.join("src/demo/Caller.java"));
        let byte = caller.find("api.list(1)").unwrap() + 4;
        let id = index
            .symbol_at(&uri, position_at(caller, byte))
            .expect("call should resolve");
        let symbol = index.symbol(id).unwrap();
        assert_eq!(symbol.owner_fqn.as_deref(), Some("demo.Api"));
        assert_eq!(symbol.descriptor, "(int a)");
    }

    #[test]
    fn builtin_excludes_can_be_reincluded_by_negation() {
        let rules = ScanRules::default();
        assert!(rules.is_excluded_dir("target"));
        assert!(rules.is_excluded_dir("node_modules"));
        // A `!` rule wins over a builtin, while `.ld-java-analyze` stays protected.
        let reenabled = ScanRules::new(
            &["!target".to_string(), "!.ld-java-analyze".to_string()],
            true,
        );
        assert!(!reenabled.is_excluded_dir("target"));
        assert!(reenabled.is_excluded_dir(".ld-java-analyze"));
    }

    #[test]
    fn exclude_rules_filter_directories_and_files_and_reload() {
        let workspace = TempWorkspace::new("excludes");
        workspace.write("src/demo/Keep.java", "package demo; class Keep {}");
        workspace.write(
            "src/demo/Skip.generated.java",
            "package demo; class SkipGenerated {}",
        );
        workspace.write("generated/Noise.java", "package demo; class Noise {}");

        let mut index = WorkspaceIndex::open(&workspace.0, &[]);
        assert_eq!(index.scan().files, 3);
        assert_eq!(index.symbols_named("Noise").len(), 1);

        let config = ScanConfig {
            reference_roots: Vec::new(),
            exclude_rules: vec!["generated/".into(), "file:*.generated.java".into()],
            use_builtin_excludes: true,
        };
        assert_eq!(index.set_scan_config(&config).files, 1);
        assert!(index.symbols_named("Noise").is_empty());
        assert!(index.symbols_named("SkipGenerated").is_empty());
        assert_eq!(index.symbols_named("Keep").len(), 1);

        // `.ld-java-analyze/ignore` is re-read on every scan; `!` re-includes a file.
        fs::create_dir_all(workspace.0.join(".ld-java-analyze")).unwrap();
        fs::write(
            workspace.0.join(".ld-java-analyze/ignore"),
            "# keep generated files\n!file:*.generated.java\n",
        )
        .unwrap();
        assert_eq!(index.scan().files, 2);
        assert_eq!(index.symbols_named("SkipGenerated").len(), 1);
    }

    #[test]
    fn mybatis_links_connect_xml_and_interface() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("testdata")
            .join("demo1");
        let mut index = WorkspaceIndex::open(&root, &[]);
        let report = index.scan();
        assert!(report.links >= 2, "report={report:?}");
        assert!(index
            .links()
            .iter()
            .any(|link| link.kind == "mybatis.mapper-xml"));
    }

    #[test]
    fn snapshot_round_trip_keeps_files_and_symbols() {
        let workspace = TempWorkspace::new("snapshot");
        workspace.write("src/demo/UserService.java", SERVICE);

        let mut index = WorkspaceIndex::open(&workspace.0, &["src".to_string()]);
        index.scan();
        let before = index.all_symbols().len();
        index.save().unwrap();

        let restored = WorkspaceIndex::open(&workspace.0, &["src".to_string()]);
        assert_eq!(restored.all_symbols().len(), before);
        assert_eq!(restored.reference_roots().len(), 1);

        // Explicit caller configuration always wins over the stale snapshot.
        let overridden = WorkspaceIndex::open(&workspace.0, &[]);
        assert!(overridden.reference_roots().is_empty());
    }
}
