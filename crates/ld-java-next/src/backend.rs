//! Independent LSP backend for the next-generation Java navigation index.

#![allow(deprecated)]

use nav_next::{
    hash_content, FileChange, FileChangeDetector, FileContribution, FileFingerprint, FileId,
    IndexBuilder, IndexStore, JavaAnalyzer, Position, SnapshotStore, StoredFile, SymbolKind,
    WorkspaceFiles,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tower_lsp_server::jsonrpc::{Error as RpcError, Result as LspResult};
use tower_lsp_server::ls_types as lsp;
use tower_lsp_server::ls_types::request::{
    GotoImplementationParams, GotoImplementationResponse, GotoTypeDefinitionParams,
    GotoTypeDefinitionResponse,
};
use tower_lsp_server::{Client, LanguageServer};

const READ_BATCH_SIZE: usize = 50;
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const EDIT_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Debug, Deserialize, Serialize)]
struct ClientDocument {
    uri: String,
    text: Option<String>,
    error: Option<String>,
}

struct ReadTextDocuments;

impl lsp::request::Request for ReadTextDocuments {
    type Params = Value;
    type Result = Vec<ClientDocument>;
    const METHOD: &'static str = "ld-java-next/readTextDocuments";
}

#[derive(Debug, Serialize, Deserialize)]
struct ScanProgressParams {
    phase: String,
    processed: usize,
    total: usize,
}

struct ScanProgress;

impl lsp::notification::Notification for ScanProgress {
    type Params = ScanProgressParams;
    const METHOD: &'static str = "ld-java-next/scanProgress";
}

#[derive(Debug)]
struct State {
    root: PathBuf,
    files: WorkspaceFiles,
    documents: nav_next::DocumentStore,
    builder: IndexBuilder,
    store: IndexStore,
    fingerprints: FileChangeDetector,
}

impl State {
    fn new(root: PathBuf) -> Self {
        let store_path = root.join(".ld-java-next").join("index.redb");
        let store = IndexStore::open(store_path).expect("open ld-java-next index store");
        Self {
            root,
            files: WorkspaceFiles::new(),
            documents: nav_next::DocumentStore::new(),
            builder: IndexBuilder::new(),
            store,
            fingerprints: FileChangeDetector::new(),
        }
    }
}

#[derive(Clone)]
pub struct Backend {
    client: Client,
    state: Arc<Mutex<State>>,
    snapshots: Arc<SnapshotStore>,
    path_to_file: Arc<StdRwLock<HashMap<String, FileId>>>,
    file_to_uri: Arc<StdRwLock<HashMap<FileId, lsp::Uri>>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        let root = std::env::temp_dir();
        let state = State::new(root);
        Self {
            client,
            state: Arc::new(Mutex::new(state)),
            snapshots: Arc::new(SnapshotStore::new(None)),
            path_to_file: Arc::new(StdRwLock::new(HashMap::new())),
            file_to_uri: Arc::new(StdRwLock::new(HashMap::new())),
        }
    }

    async fn register_file(&self, path: &Path) -> FileId {
        let file_id = {
            let mut state = self.state.lock().await;
            state.files.register_path(path)
        };
        self.path_to_file
            .write()
            .expect("path map lock")
            .insert(normalize_path(path), file_id);
        if let Some(uri) = uri_from_path(path) {
            self.file_to_uri
                .write()
                .expect("file uri map lock")
                .insert(file_id, uri);
        }
        file_id
    }

    async fn file_id_of(&self, uri: &lsp::Uri) -> Option<FileId> {
        let path = self.path_of(uri).await?;
        self.path_to_file
            .read()
            .expect("path map lock")
            .get(&normalize_path(&path))
            .copied()
    }

    async fn path_of(&self, uri: &lsp::Uri) -> Option<PathBuf> {
        uri.to_file_path()
            .map(|path| path.into_owned())
            .or_else(|| {
                uri.to_string()
                    .strip_prefix("file:///")
                    .map(|raw| PathBuf::from(raw.replace('/', std::path::MAIN_SEPARATOR_STR)))
            })
    }

    async fn load_existing_index(&self) {
        let records = {
            let state = self.state.lock().await;
            state.store.load_files().unwrap_or_default()
        };
        let mut state = self.state.lock().await;
        for record in records {
            let path = record.path.clone();
            let file_id = state.files.register_path(&path);
            state.fingerprints.observe(record.fingerprint.clone());
            let _ = state.builder.add_file(record.contribution.clone());
            let _ = self
                .path_to_file
                .write()
                .expect("path map lock")
                .insert(normalize_path(&path), file_id);
            if let Some(uri) = uri_from_path(&path) {
                let _ = self
                    .file_to_uri
                    .write()
                    .expect("file uri map lock")
                    .insert(file_id, uri);
            }
        }
        if let Ok(snapshot) = state.builder.build() {
            self.snapshots.publish(snapshot);
        }
    }

    async fn scan_workspace(&self, force: bool) {
        self.scan_workspace_scoped(None, force).await;
    }

    async fn scan_directory(&self, path: &Path, force: bool) {
        self.scan_workspace_scoped(Some(path), force).await;
    }

    async fn scan_workspace_scoped(&self, scope: Option<&Path>, force: bool) {
        let started = Instant::now();
        let root = self.state.lock().await.root.clone();
        let scan_root = scope.map(Path::to_path_buf).unwrap_or_else(|| root.clone());
        let paths = collect_java_files(&scan_root);
        let total_files = paths.len();
        self.send_scan_progress("discover", 0, total_files).await;

        let mut pending_texts: HashMap<PathBuf, Arc<str>> = HashMap::new();
        let mut unreadable: Vec<String> = Vec::new();
        let mut seen_paths: HashSet<PathBuf> = HashSet::new();

        for (index, path) in paths.iter().enumerate() {
            if index % 500 == 0 && index > 0 {
                self.send_scan_progress("read", index, total_files).await;
            }
            seen_paths.insert(path.clone());
            let file_id = self.register_file(path).await;
            let uri = uri_from_path(path)
                .map(|uri| uri.to_string())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());

            let open_text = {
                let state = self.state.lock().await;
                state.documents.text(file_id)
            };

            if let Some(text) = open_text {
                pending_texts.insert(path.clone(), text);
                continue;
            }

            if !force {
                let metadata = tokio::fs::metadata(&path).await.ok();
                let size = metadata.as_ref().map(|data| data.len()).unwrap_or_default();
                let mtime_ns = metadata
                    .and_then(|data| data.modified().ok())
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos())
                    .unwrap_or_default();
                let unchanged = {
                    let state = self.state.lock().await;
                    state
                        .fingerprints
                        .current(file_id)
                        .map(|fingerprint| {
                            fingerprint.size == size && fingerprint.mtime_ns == mtime_ns
                        })
                        .unwrap_or(false)
                };
                if unchanged {
                    continue;
                }
            }

            match tokio::fs::read_to_string(&path).await {
                Ok(text) => {
                    pending_texts.insert(path.clone(), Arc::from(text.as_str()));
                }
                Err(_) => unreadable.push(uri),
            }
        }

        let client_texts = self.request_client_texts(unreadable).await;
        self.send_scan_progress("index", 0, pending_texts.len())
            .await;
        for (uri, text) in client_texts {
            if let Some(path) = self.path_from_uri_string(&uri).await {
                seen_paths.insert(path.clone());
                pending_texts.insert(path, Arc::from(text.as_str()));
            }
        }

        let mut contributions: Vec<FileContribution> = Vec::new();
        let mut changed_records: Vec<StoredFile> = Vec::new();
        let mut pending_texts: Vec<(PathBuf, Arc<str>)> = pending_texts.into_iter().collect();
        pending_texts.sort_by(|left, right| left.0.cmp(&right.0));
        let total_pending = pending_texts.len();

        for (index, (path, text)) in pending_texts.into_iter().enumerate() {
            if index % 500 == 0 && index > 0 {
                self.send_scan_progress("index", index, total_pending).await;
            }
            let file_id = self.register_file(&path).await;
            let metadata = tokio::fs::metadata(&path).await.ok();
            let size = metadata.as_ref().map(|data| data.len()).unwrap_or_default();
            let mtime_ns = metadata
                .and_then(|data| data.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            let fingerprint = FileFingerprint {
                file_id,
                size,
                mtime_ns,
                hash: hash_content(&text),
            };
            let change = {
                let mut state = self.state.lock().await;
                state.fingerprints.observe(fingerprint.clone())
            };
            if change == FileChange::Unchanged && !force {
                continue;
            }
            let analyzer = JavaAnalyzer::new();
            if let Ok(contribution) = analyzer.analyze(file_id, &text) {
                changed_records.push(StoredFile {
                    path: path.clone(),
                    contribution: contribution.clone(),
                    fingerprint,
                });
                contributions.push(contribution);
            }
        }

        {
            let mut state = self.state.lock().await;
            if scope.is_none() {
                let live: HashSet<FileId> = seen_paths
                    .iter()
                    .filter_map(|path| state.files.file_id_of(path))
                    .collect();
                let removed: Vec<FileId> = state
                    .builder
                    .file_ids()
                    .filter(|file| !live.contains(file))
                    .collect();
                for file in removed {
                    let _ = state.builder.remove_file(file);
                    let _ = state.store.remove_file(file);
                    let _ = state.fingerprints.remove(file);
                }
            }

            for contribution in contributions {
                let _ = state.builder.replace_file(contribution);
            }
            for record in &changed_records {
                let _ = state.store.save_file(record);
            }
            if let Ok(snapshot) = state.builder.build() {
                self.snapshots.publish(snapshot);
            }
        }

        let elapsed = started.elapsed().as_millis();
        self.send_scan_progress("complete", total_files, total_files)
            .await;
        self.client
            .log_message(
                lsp::MessageType::INFO,
                format!("ld-java-next: workspace scan finished in {elapsed}ms"),
            )
            .await;
    }

    async fn send_scan_progress(&self, phase: &str, processed: usize, total: usize) {
        self.client
            .send_notification::<ScanProgress>(ScanProgressParams {
                phase: phase.to_string(),
                processed,
                total,
            })
            .await;
    }

    async fn request_client_texts(&self, uris: Vec<String>) -> HashMap<String, String> {
        let mut out = HashMap::new();
        let total = uris.len();
        let mut processed = 0;
        for batch in uris.chunks(READ_BATCH_SIZE) {
            let response = tokio::time::timeout(
                READ_TIMEOUT,
                self.client
                    .send_request::<ReadTextDocuments>(json!({ "uris": batch })),
            )
            .await;
            if let Ok(Ok(documents)) = response {
                for document in documents {
                    if let Some(text) = document.text {
                        out.insert(document.uri, text);
                    }
                }
            }
            processed += batch.len();
            self.send_scan_progress("clientRead", processed, total)
                .await;
        }
        out
    }

    async fn path_from_uri_string(&self, uri: &str) -> Option<PathBuf> {
        if let Ok(parsed) = uri.parse::<lsp::Uri>() {
            return self.path_of(&parsed).await;
        }
        uri.strip_prefix("file:///")
            .map(|raw| PathBuf::from(raw.replace('/', std::path::MAIN_SEPARATOR_STR)))
    }

    async fn apply_document(&self, file_id: FileId, text: impl Into<Arc<str>>) {
        let text = text.into();
        let path = {
            let state = self.state.lock().await;
            state.files.path_of(file_id).map(|path| path.to_path_buf())
        };

        let contribution = {
            let mut state = self.state.lock().await;
            let hash = hash_content(&text);
            let metadata = path
                .as_deref()
                .and_then(|path| std::fs::metadata(path).ok());
            let size = metadata.as_ref().map(|data| data.len()).unwrap_or_default();
            let mtime_ns = metadata
                .and_then(|data| data.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            let fingerprint = FileFingerprint {
                file_id,
                size,
                mtime_ns,
                hash,
            };
            state.fingerprints.observe(fingerprint.clone());
            let analyzer = JavaAnalyzer::new();
            let contribution = analyzer.analyze(file_id, &text).ok();
            if let Some(contribution) = contribution.clone() {
                let _ = state.builder.replace_file(contribution.clone());
                if let Some(path) = path.as_deref() {
                    let _ = state.store.save_file(&StoredFile {
                        path: path.to_path_buf(),
                        contribution: contribution.clone(),
                        fingerprint,
                    });
                }
            }
            contribution
        };

        if contribution.is_some() {
            self.rebuild_snapshot().await;
        }
    }

    async fn rebuild_snapshot(&self) {
        let snapshot = {
            let mut state = self.state.lock().await;
            state.builder.build()
        };
        if let Ok(snapshot) = snapshot {
            self.snapshots.publish(snapshot);
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: lsp::InitializeParams) -> LspResult<lsp::InitializeResult> {
        let root = params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .and_then(|folder| folder.uri.to_file_path())
            .map(|path| path.into_owned())
            .or_else(|| {
                params
                    .root_uri
                    .as_ref()
                    .and_then(|uri| uri.to_file_path())
                    .map(|path| path.into_owned())
            })
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        {
            let mut state = self.state.lock().await;
            if state.root != root {
                *state = State::new(root.clone());
                self.path_to_file.write().expect("path map lock").clear();
                self.file_to_uri.write().expect("file uri map lock").clear();
            }
        }

        self.load_existing_index().await;

        Ok(lsp::InitializeResult {
            capabilities: lsp::ServerCapabilities {
                text_document_sync: Some(lsp::TextDocumentSyncCapability::Kind(
                    lsp::TextDocumentSyncKind::FULL,
                )),
                definition_provider: Some(lsp::OneOf::Left(true)),
                type_definition_provider: Some(lsp::TypeDefinitionProviderCapability::Simple(true)),
                implementation_provider: Some(lsp::ImplementationProviderCapability::Simple(true)),
                references_provider: Some(lsp::OneOf::Left(true)),
                document_symbol_provider: Some(lsp::OneOf::Left(true)),
                workspace_symbol_provider: Some(lsp::OneOf::Left(true)),
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
        let backend = self.clone();
        tokio::spawn(async move {
            backend.scan_workspace(false).await;
        });
        self.client
            .log_message(
                lsp::MessageType::INFO,
                "ld-java-next: ready, scanning workspace in background",
            )
            .await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        let state = self.state.lock().await;
        let records = state.store.load_files().unwrap_or_default();
        for record in records {
            let _ = state.store.save_file(&record);
        }
        Ok(())
    }

    async fn did_open(&self, params: lsp::DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let version = params.text_document.version;
        if let Some(path) = self.path_of(&uri).await {
            let file_id = self.register_file(&path).await;
            {
                let mut state = self.state.lock().await;
                let _ = state
                    .documents
                    .open(file_id, text.clone(), version.max(0) as u64);
            }
            self.apply_document(file_id, text).await;
        }
    }

    async fn did_change(&self, params: lsp::DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        let Some(change) = params.content_changes.last() else {
            return;
        };
        let text = change.text.clone();
        if let Some(path) = self.path_of(&uri).await {
            let file_id = self.register_file(&path).await;
            {
                let mut state = self.state.lock().await;
                let _ = state
                    .documents
                    .update(file_id, text.clone(), version.max(0) as u64);
            }
            let backend = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(EDIT_DEBOUNCE).await;
                let current_version = {
                    let state = backend.state.lock().await;
                    state.documents.version(file_id)
                };
                if current_version == Some(version.max(0) as u64) {
                    backend.apply_document(file_id, text).await;
                }
            });
        }
    }

    async fn did_save(&self, params: lsp::DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(path) = self.path_of(&uri).await {
            let file_id = self.register_file(&path).await;
            let text = {
                let state = self.state.lock().await;
                state.documents.text(file_id)
            };
            if let Some(text) = text {
                self.apply_document(file_id, text).await;
            } else if let Ok(text) = tokio::fs::read_to_string(&path).await {
                self.apply_document(file_id, text).await;
            }
        }
    }

    async fn did_close(&self, params: lsp::DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(path) = self.path_of(&uri).await {
            let file_id = self.register_file(&path).await;
            {
                let mut state = self.state.lock().await;
                let _ = state.documents.close(file_id);
            }
            if let Ok(text) = tokio::fs::read_to_string(&path).await {
                self.apply_document(file_id, text).await;
            }
        }
    }

    async fn goto_definition(
        &self,
        params: lsp::GotoDefinitionParams,
    ) -> LspResult<Option<lsp::GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let query = self.snapshots.current();
        let file_id = self.file_id_of(&uri).await;
        self.client
            .log_message(
                lsp::MessageType::INFO,
                format!(
                    "definition request uri={} position={:?} file_id={:?}",
                    uri.as_str(),
                    position,
                    file_id
                ),
            )
            .await;
        if let Some(file_id) = file_id {
            let locations = query.definition_at(file_id, to_nav_position(position));
            self.client
                .log_message(
                    lsp::MessageType::INFO,
                    format!("definition result count={}", locations.len()),
                )
                .await;
            return Ok(Some(lsp::GotoDefinitionResponse::Array(
                locations
                    .into_iter()
                    .filter_map(|location| {
                        Some(lsp::Location {
                            uri: self.uri_of_file(location.file_id)?,
                            range: to_lsp_range(location.span),
                        })
                    })
                    .collect(),
            )));
        }
        Ok(None)
    }

    async fn goto_type_definition(
        &self,
        params: GotoTypeDefinitionParams,
    ) -> LspResult<Option<GotoTypeDefinitionResponse>> {
        self.goto_definition(params).await
    }

    async fn goto_implementation(
        &self,
        params: GotoImplementationParams,
    ) -> LspResult<Option<GotoImplementationResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let query = self.snapshots.current();
        let file_id = self.file_id_of(&uri).await;
        self.client
            .log_message(
                lsp::MessageType::INFO,
                format!(
                    "implementation request uri={} position={:?} file_id={:?}",
                    uri.as_str(),
                    position,
                    file_id
                ),
            )
            .await;
        if let Some(file_id) = file_id {
            let symbols = query.implementations_at(file_id, to_nav_position(position));
            self.client
                .log_message(
                    lsp::MessageType::INFO,
                    format!("implementation result count={}", symbols.len()),
                )
                .await;
            return Ok(Some(GotoImplementationResponse::Array(
                symbols
                    .into_iter()
                    .filter_map(|symbol| {
                        Some(lsp::Location {
                            uri: self.uri_of_file(symbol.file_id)?,
                            range: to_lsp_range(symbol.selection_span),
                        })
                    })
                    .collect(),
            )));
        }
        Ok(None)
    }

    async fn references(
        &self,
        params: lsp::ReferenceParams,
    ) -> LspResult<Option<Vec<lsp::Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        let query = self.snapshots.current();
        let file_id = self.file_id_of(&uri).await;
        self.client
            .log_message(
                lsp::MessageType::INFO,
                format!(
                    "references request uri={} position={:?} file_id={:?}",
                    uri.as_str(),
                    position,
                    file_id
                ),
            )
            .await;
        if let Some(file_id) = file_id {
            if let Some(symbol_id) = query.symbol_at(file_id, to_nav_position(position)) {
                let occurrences = query.references(symbol_id);
                self.client
                    .log_message(
                        lsp::MessageType::INFO,
                        format!(
                            "references result symbol={:?} count={}",
                            symbol_id,
                            occurrences.len()
                        ),
                    )
                    .await;
                let locations = occurrences
                    .into_iter()
                    .filter(|occurrence| {
                        include_declaration
                            || occurrence.role != nav_next::OccurrenceRole::Definition
                    })
                    .filter_map(|occurrence| {
                        Some(lsp::Location {
                            uri: self.uri_of_file(occurrence.file_id)?,
                            range: to_lsp_range(occurrence.span),
                        })
                    })
                    .collect();
                return Ok(Some(locations));
            }
        }
        Ok(None)
    }

    async fn document_symbol(
        &self,
        params: lsp::DocumentSymbolParams,
    ) -> LspResult<Option<lsp::DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let query = self.snapshots.current();
        if let Some(file_id) = self.file_id_of(&uri).await {
            let symbols = query.snapshot().symbols_in_file(file_id);
            let information = symbols
                .into_iter()
                .filter_map(|symbol| {
                    Some(lsp::SymbolInformation {
                        name: symbol.name,
                        kind: symbol_kind(symbol.kind),
                        tags: None,
                        deprecated: None,
                        location: lsp::Location {
                            uri: self.uri_of_file(symbol.file_id)?,
                            range: to_lsp_range(symbol.span),
                        },
                        container_name: symbol
                            .owner
                            .and_then(|owner| query.snapshot().symbol(owner))
                            .map(|owner| owner.name.clone()),
                    })
                })
                .collect();
            return Ok(Some(lsp::DocumentSymbolResponse::Flat(information)));
        }
        Ok(None)
    }

    async fn symbol(
        &self,
        params: lsp::WorkspaceSymbolParams,
    ) -> LspResult<Option<lsp::WorkspaceSymbolResponse>> {
        let query = params.query;
        let engine = self.snapshots.current();
        let symbols = engine.search_methods(&query, 100);
        let information = symbols
            .into_iter()
            .filter_map(|symbol| {
                Some(lsp::SymbolInformation {
                    name: symbol.name,
                    kind: symbol_kind(symbol.kind),
                    tags: None,
                    deprecated: None,
                    location: lsp::Location {
                        uri: self.uri_of_file(symbol.file_id)?,
                        range: to_lsp_range(symbol.selection_span),
                    },
                    container_name: symbol
                        .owner
                        .and_then(|owner| engine.snapshot().symbol(owner))
                        .map(|owner| owner.name.clone()),
                })
            })
            .collect();
        Ok(Some(lsp::WorkspaceSymbolResponse::Flat(information)))
    }

    async fn execute_command(&self, params: lsp::ExecuteCommandParams) -> LspResult<Option<Value>> {
        let command = params.command.as_str();
        match command {
            "ld-java-next.searchMethods" => {
                let query = params
                    .arguments
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let engine = self.snapshots.current();
                let symbols = engine.search_methods(query, 100);
                let results: Vec<Value> = symbols
                    .into_iter()
                    .map(|symbol| {
                        json!({
                            "name": symbol.name,
                            "fqn": symbol.fqn,
                            "descriptor": symbol.descriptor,
                            "uri": self.uri_of_file(symbol.file_id).map(|uri| uri.to_string()),
                            "range": {
                                "start": {
                                    "line": symbol.selection_span.start.line,
                                    "character": symbol.selection_span.start.character,
                                },
                                "end": {
                                    "line": symbol.selection_span.end.line,
                                    "character": symbol.selection_span.end.character,
                                },
                            },
                        })
                    })
                    .collect();
                Ok(Some(json!({ "methods": results })))
            }
            "ld-java-next.stats" => {
                let engine = self.snapshots.current();
                let state = self.state.lock().await;
                Ok(Some(json!({
                    "files": engine.snapshot().file_count(),
                    "symbols": engine.snapshot().symbol_count(),
                    "occurrences": engine.snapshot().occurrence_count(),
                    "openDocuments": state.documents.len(),
                    "generation": engine.snapshot().generation(),
                })))
            }
            "ld-java-next.reindex" => {
                self.scan_workspace(true).await;
                Ok(Some(json!({ "status": "ok" })))
            }
            "ld-java-next.reindexPaths" => {
                let path = params
                    .arguments
                    .first()
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        self.state
                            .try_lock()
                            .map(|state| state.root.clone())
                            .unwrap_or_else(|_| PathBuf::from("."))
                    });
                self.scan_directory(&path, true).await;
                Ok(Some(json!({ "status": "ok", "path": path })))
            }
            _ => Err(RpcError::method_not_found()),
        }
    }
}

impl Backend {
    fn uri_of_file(&self, file: FileId) -> Option<lsp::Uri> {
        self.file_to_uri
            .read()
            .expect("file uri map lock")
            .get(&file)
            .cloned()
    }
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn uri_from_path(path: &Path) -> Option<lsp::Uri> {
    let normalized = normalize_path(path);
    let raw = if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    };
    lsp::Uri::from_str(&raw).ok()
}

fn collect_java_files(root: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
        })
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.eq_ignore_ascii_case("java"))
                .unwrap_or(false)
        })
        .filter(|path| !path.to_string_lossy().contains(".ld-java-next"))
        .collect()
}

fn to_nav_position(position: lsp::Position) -> Position {
    Position {
        line: position.line,
        character: position.character,
    }
}

fn to_lsp_position(position: Position) -> lsp::Position {
    lsp::Position {
        line: position.line,
        character: position.character,
    }
}

fn to_lsp_range(span: nav_next::Span) -> lsp::Range {
    lsp::Range {
        start: to_lsp_position(span.start),
        end: to_lsp_position(span.end),
    }
}

fn symbol_kind(kind: SymbolKind) -> lsp::SymbolKind {
    match kind {
        SymbolKind::Class => lsp::SymbolKind::CLASS,
        SymbolKind::Interface => lsp::SymbolKind::INTERFACE,
        SymbolKind::Enum => lsp::SymbolKind::ENUM,
        SymbolKind::Annotation => lsp::SymbolKind::CLASS,
        SymbolKind::Record => lsp::SymbolKind::CLASS,
        SymbolKind::Method => lsp::SymbolKind::METHOD,
        SymbolKind::Constructor => lsp::SymbolKind::CONSTRUCTOR,
        SymbolKind::Field => lsp::SymbolKind::FIELD,
        SymbolKind::Local => lsp::SymbolKind::VARIABLE,
        SymbolKind::Parameter => lsp::SymbolKind::VARIABLE,
        SymbolKind::Synthetic => lsp::SymbolKind::METHOD,
    }
}
