//! LSP backend: definition / references / highlights / symbols / implementations
//! plus the custom commands used by the VS Code thin client.

#![allow(deprecated)]

use crate::convert::{
    annotation_owner, apply_scan_settings, canonical, canonical_string, highlight_kind, import_for,
    is_type_kind, line_location, parse_scan_config, symbol_kind, to_lsp_location, to_lsp_range,
    to_lsp_uri, to_nav_position, word_and_receiver,
};
use crate::logger::{
    log_dir_from_file, log_dir_from_settings, resolve_log_dir, Logger, RETENTION_DAYS,
};
use nav_core::{Position as NavPosition, Range as NavRange, Symbol as NavSymbol, SymbolKind};
use nav_index::{has_workspace_type, ScanReport, WorkspaceIndex};
use nav_jar::{CandidateKind, JarIndex};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::Instant;
use tokio::sync::RwLock;
use tower_lsp_server::jsonrpc::{Error as JsonRpcError, ErrorCode, Result as LspResult};
use tower_lsp_server::ls_types as lsp;
use tower_lsp_server::{Client, LanguageServer};

pub const COMMANDS: &[&str] = &[
    "ld-java-analyze.reindex",
    "ld-java-analyze.setReferenceRoots",
    "ld-java-analyze.referencesInPaths",
    "ld-java-analyze.jarCandidates",
    "ld-java-analyze.setExcludeRules",
    "ld-java-analyze.getExcludeRules",
    "ld-java-analyze.setLogDir",
    "ld-java-analyze.stats",
];

pub struct State {
    pub index: WorkspaceIndex,
    pub jars: JarIndex,
    /// Open documents override disk contents. Keys are canonical `file_uri` strings.
    pub documents: HashMap<String, String>,
}

impl State {
    fn document_text(&self, uri: &str) -> Option<String> {
        self.documents
            .get(uri)
            .cloned()
            .or_else(|| std::fs::read_to_string(nav_core::uri_to_path(uri)).ok())
    }
}

#[derive(Clone)]
pub struct Backend {
    client: Client,
    state: Arc<RwLock<State>>,
    /// `None` = logging disabled. Configured at `initialize` and on config changes.
    logger: Arc<StdRwLock<Option<Arc<Logger>>>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let index = WorkspaceIndex::open(&root, &[]);
        let jars = JarIndex::scan(index.context());
        Self {
            client,
            state: Arc::new(RwLock::new(State {
                index,
                jars,
                documents: HashMap::new(),
            })),
            logger: Arc::new(StdRwLock::new(None)),
        }
    }

    /// Empty config disables logging. Enabling also performs the retention sweep.
    fn configure_logger(&self, root: &Path, configured: &str) {
        let enabled =
            resolve_log_dir(root, configured).and_then(|dir| Logger::enable(dir).map(Arc::new));
        *self
            .logger
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = enabled;
    }

    fn log(&self, level: &str, scope: &str, message: impl AsRef<str>) {
        let logger = self
            .logger
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(logger) = logger {
            let message = message.as_ref();
            match level {
                "WARN" => logger.warn(scope, message),
                "ERROR" => logger.error(scope, message),
                _ => logger.info(scope, message),
            }
        }
    }

    fn log_info(&self, scope: &str, message: impl AsRef<str>) {
        self.log("INFO", scope, message);
    }

    fn log_warn(&self, scope: &str, message: impl AsRef<str>) {
        self.log("WARN", scope, message);
    }

    fn log_error(&self, scope: &str, message: impl AsRef<str>) {
        self.log("ERROR", scope, message);
    }

    fn log_report(&self, scope: &str, report: &ScanReport, elapsed_ms: u128) {
        self.log_info(
            scope,
            format!(
                "files={} changed={} symbols={} occurrences={} links={} failed={} elapsed={}ms",
                report.files,
                report.changed,
                report.symbols,
                report.occurrences,
                report.links,
                report.failed_files.len(),
                elapsed_ms
            ),
        );
        for failure in &report.failed_files {
            self.log_error(scope, format!("解析失败 {failure}"));
        }
    }

    /// Resolves a click on a library symbol to all locally available jars.
    fn library_target(
        &self,
        state: &State,
        text: &str,
        word: &str,
        receiver: Option<&str>,
        position: NavPosition,
    ) -> (String, Option<String>) {
        if word.contains('.') {
            return (word.to_string(), None);
        }
        if let Some(import) = import_for(text, word) {
            return (import, None);
        }
        if let Some(symbol) = state
            .index
            .symbols_named(word)
            .into_iter()
            .find(|symbol| is_type_kind(symbol.kind))
        {
            return (symbol.fqn, None);
        }
        if let Some(symbol) = state.index.symbols_named(word).into_iter().find(|symbol| {
            matches!(
                symbol.kind,
                SymbolKind::Method | SymbolKind::Field | SymbolKind::Synthetic
            )
        }) {
            if let Some(owner) = symbol.owner_fqn {
                return (owner, Some(word.to_string()));
            }
        }
        if let Some(receiver) = receiver {
            if let Some(import) = import_for(text, receiver) {
                return (import, Some(word.to_string()));
            }
            if let Some(symbol) = state
                .index
                .symbols_named(receiver)
                .into_iter()
                .find(|symbol| is_type_kind(symbol.kind))
            {
                return (symbol.fqn, Some(word.to_string()));
            }
            let declared_type = state
                .index
                .symbols_named(receiver)
                .into_iter()
                .find_map(|symbol| (!symbol.descriptor.is_empty()).then_some(symbol.descriptor));
            if let Some(descriptor) = declared_type {
                let simple = descriptor
                    .split('<')
                    .next()
                    .unwrap_or(&descriptor)
                    .split('[')
                    .next()
                    .unwrap_or(&descriptor)
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .trim();
                if let Some(import) = import_for(text, simple) {
                    return (import, Some(word.to_string()));
                }
                if let Some(symbol) = state
                    .index
                    .symbols_named(simple)
                    .into_iter()
                    .find(|symbol| is_type_kind(symbol.kind))
                {
                    return (symbol.fqn, Some(word.to_string()));
                }
            }
        }
        // Annotation attribute names (`@Operation(summary = ...)`) have no
        // import of their own; resolve them through the enclosing annotation type.
        if let Some(owner) = annotation_owner(text, position) {
            if let Some(owner_fqn) = import_for(text, &owner) {
                let member = (word != owner).then(|| word.to_string());
                return (owner_fqn, member);
            }
        }
        (word.to_string(), None)
    }

    fn library_locations(
        &self,
        state: &State,
        uri: &str,
        position: NavPosition,
    ) -> Vec<lsp::Location> {
        let Some(text) = state.document_text(uri) else {
            self.log_warn("definition", format!("库回退跳过：读不到文档内容 {uri}"));
            return Vec::new();
        };
        let Some((word, receiver)) = word_and_receiver(&text, position) else {
            self.log_warn(
                "definition",
                format!(
                    "库回退跳过：光标下无标识符 {uri}:{}:{}",
                    position.line, position.character
                ),
            );
            return Vec::new();
        };
        let (fqn, member) = self.library_target(state, &text, &word, receiver.as_deref(), position);
        self.log_info(
            "definition",
            format!(
                "库回退 word={word} receiver={receiver:?} fqn={fqn} member={member:?}",
                receiver = receiver.as_deref(),
            ),
        );
        // A type that has workspace sources must never resolve into a jar. If our
        // parser could not bind the member, returning no target is preferable to
        // jumping into a decompiled/structure view of a class the user can open.
        let simple = fqn.rsplit('.').next().unwrap_or(&fqn);
        if has_workspace_type(&state.index.symbols_named(simple), simple) {
            self.log_info(
                "definition",
                format!("工作区已有类型 {simple}，不降级到 JAR"),
            );
            return Vec::new();
        }
        let mut candidates = state.jars.candidates(&fqn, member.as_deref());
        // Source-preferred navigation: only fall back to a bytecode structure
        // view when no `*-sources.jar` provides this class. Returning both makes
        // the editor show a picker (or land on the structure view) instead of
        // simply opening the source.
        let source_count = candidates
            .iter()
            .filter(|candidate| candidate.kind == CandidateKind::Source)
            .count();
        if source_count > 0 {
            candidates.retain(|candidate| candidate.kind == CandidateKind::Source);
        }
        let mut out = Vec::new();
        for candidate in &candidates {
            match state.jars.materialize(candidate, state.index.state_dir()) {
                Some(materialized) => {
                    if let Some(location) = line_location(&materialized.uri, materialized.line) {
                        out.push(location);
                    }
                }
                None => self.log_warn(
                    "definition",
                    format!(
                        "无法生成目标 {} ({})",
                        candidate.label,
                        candidate.jar.display()
                    ),
                ),
            }
        }
        self.log_info(
            "definition",
            format!(
                "库候选 源码={source_count} 结构视图={} 最终目标={}",
                candidates.len().saturating_sub(source_count),
                out.len()
            ),
        );
        out
    }

    /// A click on a framework link (e.g. an XML statement) is mapped back to the
    /// symbol at the other end so references/hover keep working.
    fn symbol_or_link(&self, state: &State, uri: &str, position: NavPosition) -> Option<u64> {
        if let Some(id) = state.index.symbol_at(uri, position) {
            return Some(id);
        }
        for target in state.index.link_targets(uri, position) {
            if let Some(id) = state.index.symbol_at(&target.uri, target.range.start) {
                return Some(id);
            }
        }
        None
    }
}

fn resolve_root(params: &lsp::InitializeParams) -> std::path::PathBuf {
    if let Some(folders) = &params.workspace_folders {
        if let Some(folder) = folders.first() {
            if let Some(path) = folder.uri.to_file_path() {
                return path.to_path_buf();
            }
        }
    }
    if let Some(root) = &params.root_uri {
        if let Some(path) = root.to_file_path() {
            return path.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

fn dedup(locations: Vec<lsp::Location>) -> Vec<lsp::Location> {
    let mut seen = HashSet::new();
    locations
        .into_iter()
        .filter(|location| {
            seen.insert((
                location.uri.as_str().to_string(),
                location.range.start.line,
                location.range.start.character,
            ))
        })
        .collect()
}

fn symbol_information(symbol: &NavSymbol) -> Option<lsp::SymbolInformation> {
    Some(lsp::SymbolInformation {
        name: symbol.display_name(),
        kind: symbol_kind(symbol.kind),
        tags: None,
        deprecated: None,
        location: lsp::Location {
            uri: to_lsp_uri(&symbol.uri)?,
            range: to_lsp_range(symbol.range),
        },
        container_name: symbol.owner_fqn.clone(),
    })
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: lsp::InitializeParams) -> LspResult<lsp::InitializeResult> {
        let started = Instant::now();
        let root = resolve_root(&params);
        let config = parse_scan_config(params.initialization_options.as_ref());
        // VS Code always sends `logDir: ""` when the setting is unset. Treat an
        // empty client value as "not specified" so the workspace config file can
        // still enable logging; an explicit `setLogDir("")` disables it at runtime.
        let configured_log = params
            .initialization_options
            .as_ref()
            .and_then(log_dir_from_settings)
            .filter(|value| !value.trim().is_empty())
            .or_else(|| log_dir_from_file(&root.join(".ld-java-analyze")))
            .unwrap_or_default();
        self.configure_logger(&root, &configured_log);
        self.log_info(
            "startup",
            format!(
                "LSP 启动 root={} stateDir={} logDir={:?} 保留 {RETENTION_DAYS} 天",
                root.display(),
                root.join(".ld-java-analyze").display(),
                configured_log
            ),
        );
        let (message, report) = {
            let mut state = self.state.write().await;
            let mut index = WorkspaceIndex::open_with_config(&root, &config);
            let report = index.scan();
            let _ = index.save();
            state.jars = JarIndex::scan(index.context());
            self.log_info(
                "startup",
                format!(
                    "classpath={} sourceJars={} compiledJars={} referenceRoots={:?} excludeRules={:?} builtinExcludes={}",
                    index.context().classpath.len(),
                    state.jars.source_jars().len(),
                    state.jars.compiled_jars().len(),
                    config.reference_roots,
                    config.exclude_rules,
                    config.use_builtin_excludes
                ),
            );
            state.index = index;
            state.documents.clear();
            (
                format!(
                    "ld-java-analyze ready: {} files, {} symbols, {} occurrences, {} links",
                    report.files, report.symbols, report.occurrences, report.links
                ),
                report,
            )
        };
        self.log_report("index", &report, started.elapsed().as_millis());
        self.client
            .log_message(lsp::MessageType::INFO, message)
            .await;
        Ok(lsp::InitializeResult {
            capabilities: lsp::ServerCapabilities {
                text_document_sync: Some(lsp::TextDocumentSyncCapability::Kind(
                    lsp::TextDocumentSyncKind::FULL,
                )),
                definition_provider: Some(lsp::OneOf::Left(true)),
                type_definition_provider: Some(lsp::TypeDefinitionProviderCapability::Simple(true)),
                implementation_provider: Some(lsp::ImplementationProviderCapability::Simple(true)),
                references_provider: Some(lsp::OneOf::Left(true)),
                document_highlight_provider: Some(lsp::OneOf::Left(true)),
                document_symbol_provider: Some(lsp::OneOf::Left(true)),
                workspace_symbol_provider: Some(lsp::OneOf::Left(true)),
                hover_provider: Some(lsp::HoverProviderCapability::Simple(true)),
                // Keep the advertised list empty on purpose. vscode-languageclient's
                // ExecuteCommandFeature registers a VS Code command for every entry
                // here and forwards raw arguments straight to the server, which
                // shadows the extension's own UI (QuickPick) commands. The extension
                // registers all ld-java-analyze.* commands itself and calls
                // `workspace/executeCommand` programmatically instead.
                execute_command_provider: Some(lsp::ExecuteCommandOptions {
                    commands: Vec::new(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: lsp::InitializedParams) {
        self.client
            .log_message(
                lsp::MessageType::INFO,
                "ld-java-analyze: 索引位于 <workspace>/.ld-java-analyze，不会启动 Maven 或下载源码",
            )
            .await;
    }

    async fn did_change_configuration(&self, params: lsp::DidChangeConfigurationParams) {
        let mut config = {
            let state = self.state.read().await;
            state.index.scan_config().clone()
        };
        let root = {
            let state = self.state.read().await;
            state.index.root().to_path_buf()
        };
        if let Some(log_dir) = log_dir_from_settings(&params.settings) {
            // An empty value means "no client override", not "disable": fall back
            // to the workspace config file, matching `initialize`.
            let effective = if log_dir.trim().is_empty() {
                log_dir_from_file(&root.join(".ld-java-analyze")).unwrap_or_default()
            } else {
                log_dir
            };
            self.configure_logger(&root, &effective);
            self.log_info("config", format!("logDir 变更为 {effective:?}"));
        }
        apply_scan_settings(&mut config, &params.settings);
        let started = Instant::now();
        let report = {
            let mut state = self.state.write().await;
            let report = state.index.set_scan_config(&config);
            let _ = state.index.save();
            report
        };
        self.log_report("reindex", &report, started.elapsed().as_millis());
        self.client
            .log_message(
                lsp::MessageType::INFO,
                format!(
                    "ld-java-analyze: 扫描规则已更新，索引 {report} 个文件 / {links} 条框架链接",
                    report = report.files,
                    links = report.links
                ),
            )
            .await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        self.log_info("shutdown", "LSP 关闭，保存索引快照");
        let state = self.state.read().await;
        let _ = state.index.save();
        Ok(())
    }

    async fn did_open(&self, params: lsp::DidOpenTextDocumentParams) {
        let uri = canonical(&params.text_document.uri);
        let text = params.text_document.text;
        let mut state = self.state.write().await;
        state.documents.insert(uri.clone(), text.clone());
        match state.index.update_document(&uri, &text) {
            Ok(()) => self.log_info("document", format!("didOpen {} bytes={}", uri, text.len())),
            Err(error) => self.log_error("document", format!("didOpen 解析失败 {uri}: {error}")),
        }
    }

    async fn did_change(&self, params: lsp::DidChangeTextDocumentParams) {
        let uri = canonical(&params.text_document.uri);
        let Some(change) = params.content_changes.last() else {
            return;
        };
        let mut state = self.state.write().await;
        state.documents.insert(uri.clone(), change.text.clone());
        let _ = state.index.update_document(&uri, &change.text);
    }

    async fn did_save(&self, params: lsp::DidSaveTextDocumentParams) {
        let uri = canonical(&params.text_document.uri);
        let mut state = self.state.write().await;
        state.documents.remove(&uri);
        state.index.reload_from_disk(&uri);
        let _ = state.index.save();
        state.jars = JarIndex::scan(state.index.context());
        self.log_info(
            "document",
            format!(
                "didSave {} sourceJars={} compiledJars={}",
                uri,
                state.jars.source_jars().len(),
                state.jars.compiled_jars().len()
            ),
        );
    }

    async fn did_close(&self, params: lsp::DidCloseTextDocumentParams) {
        let uri = canonical(&params.text_document.uri);
        let mut state = self.state.write().await;
        state.documents.remove(&uri);
        state.index.reload_from_disk(&uri);
    }

    async fn goto_definition(
        &self,
        params: lsp::GotoDefinitionParams,
    ) -> LspResult<Option<lsp::GotoDefinitionResponse>> {
        let started = Instant::now();
        let uri = canonical(&params.text_document_position_params.text_document.uri);
        let position = to_nav_position(params.text_document_position_params.position);
        let state = self.state.read().await;
        let symbol_id = state.index.symbol_at(&uri, position);
        let mut targets: Vec<(String, NavRange)> = Vec::new();
        if let Some(id) = symbol_id {
            for target in state.index.definition_targets(id) {
                targets.push((target.uri, target.range));
            }
        }
        let links = state.index.link_targets(&uri, position);
        for target in &links {
            targets.push((target.uri.clone(), target.range));
        }
        let mut locations: Vec<lsp::Location> = targets
            .iter()
            .filter_map(|(uri, range)| to_lsp_location(uri, *range))
            .collect();
        let used_library = locations.is_empty();
        if used_library {
            locations = self.library_locations(&state, &uri, position);
        }
        let locations = dedup(locations);
        self.log_info(
            "definition",
            format!(
                "{}:{}:{} symbol={symbol_id:?} definitions={} links={} libraryFallback={used_library} targets={} elapsed={}ms",
                uri.rsplit('/').next().unwrap_or(&uri),
                position.line + 1,
                position.character + 1,
                targets.len().saturating_sub(links.len()),
                links.len(),
                locations.len(),
                started.elapsed().as_millis()
            ),
        );
        if symbol_id.is_none() && locations.is_empty() {
            self.log_warn(
                "definition",
                "未找到任何定义目标（检查 import / classpath / 扫描规则）",
            );
        }
        Ok((!locations.is_empty()).then_some(lsp::GotoDefinitionResponse::Array(locations)))
    }

    async fn goto_type_definition(
        &self,
        params: lsp::GotoDefinitionParams,
    ) -> LspResult<Option<lsp::GotoDefinitionResponse>> {
        let uri = canonical(&params.text_document_position_params.text_document.uri);
        let position = to_nav_position(params.text_document_position_params.position);
        let state = self.state.read().await;
        let Some(id) = state.index.symbol_at(&uri, position) else {
            return Ok(None);
        };
        let locations: Vec<lsp::Location> = state
            .index
            .type_definition(id)
            .iter()
            .filter_map(|symbol| to_lsp_location(&symbol.uri, symbol.selection_range))
            .collect();
        Ok((!locations.is_empty()).then_some(lsp::GotoDefinitionResponse::Array(dedup(locations))))
    }

    async fn goto_implementation(
        &self,
        params: lsp::GotoDefinitionParams,
    ) -> LspResult<Option<lsp::GotoDefinitionResponse>> {
        let uri = canonical(&params.text_document_position_params.text_document.uri);
        let position = to_nav_position(params.text_document_position_params.position);
        let state = self.state.read().await;
        let Some(id) = self.symbol_or_link(&state, &uri, position) else {
            return Ok(None);
        };
        let locations: Vec<lsp::Location> = state
            .index
            .implementations(id)
            .iter()
            .filter_map(|symbol| to_lsp_location(&symbol.uri, symbol.selection_range))
            .collect();
        Ok((!locations.is_empty()).then_some(lsp::GotoDefinitionResponse::Array(dedup(locations))))
    }

    async fn references(
        &self,
        params: lsp::ReferenceParams,
    ) -> LspResult<Option<Vec<lsp::Location>>> {
        let uri = canonical(&params.text_document_position.text_document.uri);
        let position = to_nav_position(params.text_document_position.position);
        let state = self.state.read().await;
        let Some(id) = self.symbol_or_link(&state, &uri, position) else {
            return Ok(None);
        };
        let occurrences = state
            .index
            .references(id, None, params.context.include_declaration);
        self.log_info(
            "references",
            format!(
                "symbol={id} roots={:?} includeDeclaration={} results={}",
                state.index.reference_roots(),
                params.context.include_declaration,
                occurrences.len()
            ),
        );
        let locations: Vec<lsp::Location> = occurrences
            .iter()
            .filter_map(|occurrence| to_lsp_location(&occurrence.uri, occurrence.range))
            .collect();
        Ok(Some(dedup(locations)))
    }

    async fn document_highlight(
        &self,
        params: lsp::DocumentHighlightParams,
    ) -> LspResult<Option<Vec<lsp::DocumentHighlight>>> {
        let uri = canonical(&params.text_document_position_params.text_document.uri);
        let position = to_nav_position(params.text_document_position_params.position);
        let state = self.state.read().await;
        let Some(id) = state.index.symbol_at(&uri, position) else {
            return Ok(None);
        };
        Ok(Some(
            state
                .index
                .highlights(&uri, id)
                .iter()
                .map(|occurrence| lsp::DocumentHighlight {
                    range: to_lsp_range(occurrence.range),
                    kind: Some(highlight_kind(occurrence.role)),
                })
                .collect(),
        ))
    }

    async fn document_symbol(
        &self,
        params: lsp::DocumentSymbolParams,
    ) -> LspResult<Option<lsp::DocumentSymbolResponse>> {
        let uri = canonical(&params.text_document.uri);
        let state = self.state.read().await;
        let symbols: Vec<lsp::SymbolInformation> = state
            .index
            .symbols_in_file(&uri)
            .iter()
            .filter_map(symbol_information)
            .collect();
        Ok(Some(lsp::DocumentSymbolResponse::Flat(symbols)))
    }

    async fn symbol(
        &self,
        params: lsp::WorkspaceSymbolParams,
    ) -> LspResult<Option<lsp::WorkspaceSymbolResponse>> {
        let state = self.state.read().await;
        let symbols: Vec<lsp::SymbolInformation> = state
            .index
            .workspace_symbols(&params.query, 300)
            .iter()
            .filter_map(symbol_information)
            .collect();
        Ok(Some(lsp::WorkspaceSymbolResponse::Flat(symbols)))
    }

    async fn hover(&self, params: lsp::HoverParams) -> LspResult<Option<lsp::Hover>> {
        let uri = canonical(&params.text_document_position_params.text_document.uri);
        let position = to_nav_position(params.text_document_position_params.position);
        let state = self.state.read().await;
        let Some(id) = state.index.symbol_at(&uri, position) else {
            return Ok(None);
        };
        let Some(symbol) = state.index.symbol(id) else {
            return Ok(None);
        };
        Ok(Some(lsp::Hover {
            contents: lsp::HoverContents::Scalar(lsp::MarkedString::String(format!(
                "{} {}\n{:?} · {:?}{}",
                match symbol.kind {
                    SymbolKind::Method | SymbolKind::Synthetic => "method",
                    SymbolKind::Field => "field",
                    _ => "type",
                },
                symbol.display_name(),
                symbol.kind,
                symbol.origin,
                if symbol.synthetic {
                    " · Lombok 合成符号"
                } else {
                    ""
                }
            ))),
            range: Some(to_lsp_range(symbol.selection_range)),
        }))
    }

    async fn execute_command(&self, params: lsp::ExecuteCommandParams) -> LspResult<Option<Value>> {
        let started = Instant::now();
        let argument = params.arguments.first().cloned().unwrap_or(Value::Null);
        let args_preview: String = argument.to_string().chars().take(300).collect();
        self.log_info("command", format!("{} args={args_preview}", params.command));
        let outcome = match params.command.as_str() {
            "ld-java-analyze.reindex" => {
                let started = Instant::now();
                let mut state = self.state.write().await;
                let report = state.index.scan();
                state.jars = JarIndex::scan(state.index.context());
                let _ = state.index.save();
                self.log_report("reindex", &report, started.elapsed().as_millis());
                Ok(Some(json!({
                    "files": report.files,
                    "changed": report.changed,
                    "symbols": report.symbols,
                    "occurrences": report.occurrences,
                    "links": report.links
                })))
            }
            "ld-java-analyze.setReferenceRoots" => {
                let roots: Vec<String> = argument
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let mut state = self.state.write().await;
                state.index.set_reference_roots(&roots);
                let _ = state.index.save();
                Ok(Some(
                    json!({ "referenceRoots": state.index.reference_roots() }),
                ))
            }
            "ld-java-analyze.referencesInPaths" => {
                // `external`/`fsPath` also appear when a VS Code Uri is forwarded
                // verbatim; accept them so the command degrades gracefully.
                let raw_uri = argument["uri"]
                    .as_str()
                    .or_else(|| argument["external"].as_str())
                    .unwrap_or_default();
                let uri = canonical_string(raw_uri);
                let position = NavPosition {
                    line: argument["line"].as_u64().unwrap_or(0) as u32,
                    character: argument["character"].as_u64().unwrap_or(0) as u32,
                };
                let roots: Vec<String> = argument["roots"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let include_declaration = argument["includeDeclaration"].as_bool().unwrap_or(true);
                let state = self.state.read().await;
                // Do not `return` here: the common completion log at the end of
                // execute_command must still be written.
                let outcome = if let Some(id) = self.symbol_or_link(&state, &uri, position) {
                    let occurrences = state
                        .index
                        .references(id, Some(&roots), include_declaration);
                    let locations: Vec<Value> = occurrences
                        .iter()
                        .filter_map(|occurrence| {
                            let location = to_lsp_location(&occurrence.uri, occurrence.range)?;
                            Some(json!({
                                "uri": location.uri.as_str(),
                                "range": {
                                    "start": { "line": location.range.start.line, "character": location.range.start.character },
                                    "end": { "line": location.range.end.line, "character": location.range.end.character }
                                },
                                "role": format!("{:?}", occurrence.role)
                            }))
                        })
                        .collect();
                    let total = locations.len();
                    json!({ "locations": locations, "total": total })
                } else {
                    let is_source = uri.ends_with(".java") || uri.ends_with(".xml");
                    // A normal "no result" is not a warning. In particular a
                    // probe against a non-source document (e.g. a log file) is
                    // expected and should not alarm the user.
                    self.log_info(
                        "command",
                        format!(
                            "referencesInPaths 无目标（{}）uri={uri} {}:{}",
                            if is_source {
                                "光标下无符号"
                            } else {
                                "非源码文件"
                            },
                            position.line,
                            position.character
                        ),
                    );
                    json!({ "locations": [], "total": 0 })
                };
                Ok(Some(outcome))
            }
            "ld-java-analyze.jarCandidates" => {
                let uri = canonical_string(argument["uri"].as_str().unwrap_or_default());
                let position = NavPosition {
                    line: argument["line"].as_u64().unwrap_or(0) as u32,
                    character: argument["character"].as_u64().unwrap_or(0) as u32,
                };
                let state = self.state.read().await;
                let text = state.document_text(&uri);
                let parsed = text
                    .as_deref()
                    .and_then(|value| word_and_receiver(value, position));
                // Avoid early `return`s so the common completion log is written.
                let outcome = match (text, parsed) {
                    (Some(text), Some((word, receiver))) => {
                        let (fqn, member) = self.library_target(
                            &state,
                            &text,
                            &word,
                            receiver.as_deref(),
                            position,
                        );
                        let mut candidates = Vec::new();
                        for candidate in state.jars.candidates(&fqn, member.as_deref()) {
                            let Some(materialized) =
                                state.jars.materialize(&candidate, state.index.state_dir())
                            else {
                                continue;
                            };
                            candidates.push(json!({
                                "label": candidate.label,
                                "uri": materialized.uri,
                                "line": materialized.line,
                                "jar": candidate.jar.to_string_lossy(),
                                "kind": format!("{:?}", candidate.kind),
                                "hasSource": candidate.kind == CandidateKind::Source
                            }));
                        }
                        json!({ "fqn": fqn, "member": member, "candidates": candidates })
                    }
                    (None, _) => {
                        self.log_info("command", format!("jarCandidates 读不到文档 {uri}"));
                        json!({ "fqn": "", "member": Value::Null, "candidates": [] })
                    }
                    (Some(_), None) => {
                        self.log_info(
                            "command",
                            format!(
                                "jarCandidates 光标下无标识符 {uri} {}:{}",
                                position.line, position.character
                            ),
                        );
                        json!({ "fqn": "", "member": Value::Null, "candidates": [] })
                    }
                };
                Ok(Some(outcome))
            }
            "ld-java-analyze.setExcludeRules" => {
                let rules: Vec<String> = if let Some(values) = argument.as_array() {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                } else {
                    argument["rules"]
                        .as_array()
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let mut state = self.state.write().await;
                let mut config = state.index.scan_config().clone();
                config.exclude_rules = rules;
                if let Some(flag) = argument["useBuiltinExcludes"].as_bool() {
                    config.use_builtin_excludes = flag;
                }
                let report = state.index.set_scan_config(&config);
                let _ = state.index.save();
                Ok(Some(json!({
                    "excludeRules": state.index.scan_config().exclude_rules,
                    "effectiveRules": state.index.scan_rules().patterns(),
                    "files": report.files,
                    "changed": report.changed,
                    "links": report.links
                })))
            }
            "ld-java-analyze.getExcludeRules" => {
                let state = self.state.read().await;
                Ok(Some(json!({
                    "excludeRules": state.index.scan_config().exclude_rules,
                    "effectiveRules": state.index.scan_rules().patterns(),
                    "useBuiltinExcludes": state.index.scan_config().use_builtin_excludes,
                    "ignoreFile": state.index.state_dir().join("ignore").to_string_lossy(),
                    "builtinDirs": nav_core::scan_rules::BUILTIN_EXCLUDED_DIRS,
                    "alwaysExcludedDirs": nav_core::scan_rules::ALWAYS_EXCLUDED_DIRS
                })))
            }
            "ld-java-analyze.stats" => {
                let state = self.state.read().await;
                let log_dir = self
                    .logger
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .as_ref()
                    .map(|logger| logger.dir().to_string_lossy().into_owned());
                Ok(Some(json!({
                    "root": state.index.root().to_string_lossy(),
                    "stateDir": state.index.state_dir().to_string_lossy(),
                    "files": state.index.file_count(),
                    "symbols": state.index.all_symbols().len(),
                    "occurrences": state.index.occurrence_count(),
                    "links": state.index.links().len(),
                    "sourceJars": state.jars.source_jars().len(),
                    "compiledJars": state.jars.compiled_jars().len(),
                    "referenceRoots": state.index.reference_roots(),
                    "excludeRules": state.index.scan_config().exclude_rules,
                    "effectiveRules": state.index.scan_rules().patterns(),
                    "useBuiltinExcludes": state.index.scan_config().use_builtin_excludes,
                    "logDir": log_dir,
                    "retentionDays": RETENTION_DAYS
                })))
            }
            "ld-java-analyze.setLogDir" => {
                let configured = argument
                    .as_str()
                    .or_else(|| argument["logDir"].as_str())
                    .unwrap_or("")
                    .to_string();
                let root = {
                    let state = self.state.read().await;
                    state.index.root().to_path_buf()
                };
                self.configure_logger(&root, &configured);
                self.log_info("config", format!("setLogDir -> {configured:?}"));
                Ok(Some(json!({
                    "logDir": configured,
                    "enabled": !configured.trim().is_empty(),
                    "retentionDays": RETENTION_DAYS
                })))
            }
            _ => {
                self.log_warn(
                    "command",
                    format!("未知命令 {}；支持 {COMMANDS:?}", params.command),
                );
                Err(JsonRpcError {
                    code: ErrorCode::MethodNotFound,
                    message: format!("未知命令 {}", params.command).into(),
                    data: None,
                })
            }
        };
        self.log_info(
            "command",
            format!(
                "{} 完成 elapsed={}ms ok={}",
                params.command,
                started.elapsed().as_millis(),
                outcome.is_ok()
            ),
        );
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_names_are_unique() {
        let unique: HashSet<&str> = COMMANDS.iter().copied().collect();
        assert_eq!(unique.len(), COMMANDS.len());
    }

    #[test]
    fn dedup_removes_repeated_locations() {
        let uri = lsp::Uri::from_file_path(std::path::Path::new("C:/tmp/A.java")).unwrap();
        let position = lsp::Position {
            line: 1,
            character: 0,
        };
        let location = lsp::Location {
            uri,
            range: lsp::Range {
                start: position,
                end: position,
            },
        };
        assert_eq!(dedup(vec![location.clone(), location]).len(), 1);
    }
}
