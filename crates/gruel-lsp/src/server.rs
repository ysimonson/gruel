//! `tower-lsp` `LanguageServer` impl (ADR-0091).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use gruel_compiler::PreviewFeatures;
use gruel_manifest::Manifest;
use gruel_target::Target;
use lsp_types::{
    CodeActionOrCommand, CodeActionParams, CodeActionProviderCapability, CodeActionResponse,
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DidChangeTextDocumentParams, DidChangeWatchedFilesParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentFormattingParams, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents,
    HoverParams, HoverProviderCapability, InitializeParams, InitializeResult, InitializedParams,
    InlayHint, InlayHintKind, InlayHintLabel, InlayHintParams, Location, MarkupContent, MarkupKind,
    MessageType, OneOf, ParameterInformation, ParameterLabel, Position, PositionEncodingKind,
    Range, ReferenceParams, ServerCapabilities, ServerInfo, SignatureHelp, SignatureHelpOptions,
    SignatureHelpParams, SignatureInformation, SymbolInformation, SymbolKind as LspSymbolKind,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Url, WorkspaceSymbolParams,
};
use rustc_hash::{FxHashMap, FxHashSet};
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;
use tower_lsp::{Client, LanguageServer, LspService, Server, jsonrpc};

use crate::analysis::{self, Snapshot, WorkspaceFile};
use crate::diagnostics;
use crate::document::DocState;
use crate::position::PositionEncoding;

/// Default debounce duration between the last keystroke and the next compile.
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(150);
/// Hard upper bound on a single compile pass.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// LSP backend state shared by every request handler.
pub struct Backend {
    pub client: Client,
    pub docs: Arc<DashMap<Url, DocState>>,
    /// Per-root compile snapshot, keyed by the open document's URI. Each
    /// open file is analyzed independently — its `@import` closure (the
    /// root plus every file transitively reachable through `@import`)
    /// becomes the compilation unit, so unrelated workspace files are
    /// never merged together. See [`crate::workspace::build_root_closure`].
    pub snapshots: Arc<DashMap<Url, Arc<Snapshot>>>,
    pub preview_features: PreviewFeatures,
    pub workspace_root: Arc<Mutex<Option<PathBuf>>>,
    /// ADR-0092: loaded `gruel.json` for the workspace root, when one
    /// exists. `None` falls back to per-open-buffer isolation mode
    /// (the no-manifest default).
    pub manifest: Arc<Mutex<Option<Manifest>>>,
    pub encoding: Arc<Mutex<PositionEncoding>>,
    pub analysis_tx: UnboundedSender<AnalysisRequest>,
    pub current_cancel: Arc<Mutex<Option<CancellationToken>>>,
    /// Most recent diagnostics keyed by URI. Updated atomically by the
    /// analysis worker on every successful (or partial) compile. Read by
    /// `textDocument/codeAction` to construct quick fixes for the
    /// diagnostics overlapping a requested range.
    pub last_diagnostics: Arc<DashMap<Url, Vec<Diagnostic>>>,
}

#[derive(Debug, Clone)]
pub struct AnalysisRequest {
    pub debounce: Duration,
    pub timeout: Duration,
}

impl Backend {
    pub fn new(client: Client, preview_features: PreviewFeatures) -> Self {
        let (tx, rx) = unbounded_channel();
        let me = Self {
            client: client.clone(),
            docs: Arc::new(DashMap::new()),
            snapshots: Arc::new(DashMap::new()),
            preview_features,
            workspace_root: Arc::new(Mutex::new(None)),
            manifest: Arc::new(Mutex::new(None)),
            encoding: Arc::new(Mutex::new(PositionEncoding::Utf16)),
            analysis_tx: tx,
            current_cancel: Arc::new(Mutex::new(None)),
            last_diagnostics: Arc::new(DashMap::new()),
        };
        // Spawn the analysis worker.
        let worker = AnalysisWorker {
            client,
            docs: me.docs.clone(),
            snapshots: me.snapshots.clone(),
            preview_features: me.preview_features.clone(),
            workspace_root: me.workspace_root.clone(),
            manifest: me.manifest.clone(),
            encoding: me.encoding.clone(),
            current_cancel: me.current_cancel.clone(),
            rx,
            target: Target::host(),
            published_files: DashMap::new(),
            last_diagnostics: me.last_diagnostics.clone(),
        };
        tokio::spawn(worker.run());
        me
    }

    fn queue_analysis(&self) {
        let _ = self.analysis_tx.send(AnalysisRequest {
            debounce: DEFAULT_DEBOUNCE,
            timeout: DEFAULT_TIMEOUT,
        });
    }

    /// Test-only: compile each open root synchronously and publish
    /// diagnostics. Bypasses the debounce / spawned worker so integration
    /// tests can poll a deterministic result.
    pub async fn analyze_now(&self) -> Vec<Diagnostic> {
        let workspace_root = self.workspace_root.lock().await.clone();
        let manifest = self.manifest.lock().await.clone();
        let target = Target::host();
        let mut combined_diagnostics: FxHashSet<UriDiagKey> = FxHashSet::default();
        let mut by_uri: FxHashMap<Url, Vec<Diagnostic>> = FxHashMap::default();

        let roots = collect_analysis_roots(&self.docs, manifest.as_ref());

        for (uri, root) in roots {
            let docs = self.docs.clone();
            let result = analysis::analyze_root(
                root,
                workspace_root.as_deref(),
                &self.preview_features,
                &target,
                |path| open_text_lookup(&docs, path),
            );
            if let Some(snap) = result.snapshot {
                self.snapshots.insert(uri.clone(), Arc::new(snap));
            }
            let by_file = diagnostics::group_by_file(
                result.diagnostics.into_iter(),
                workspace_root.as_deref(),
            );
            for (path, diags) in by_file {
                if let Ok(diag_uri) = Url::from_file_path(&path) {
                    for d in &diags {
                        let key = (
                            diag_uri.clone(),
                            range_key(&d.range),
                            d.message.clone(),
                            d.code
                                .as_ref()
                                .map(|c| match c {
                                    lsp_types::NumberOrString::String(s) => s.clone(),
                                    lsp_types::NumberOrString::Number(n) => n.to_string(),
                                })
                                .unwrap_or_default(),
                        );
                        if combined_diagnostics.insert(key) {
                            by_uri.entry(diag_uri.clone()).or_default().push(d.clone());
                        }
                    }
                }
            }
        }

        let mut flat = Vec::new();
        for (uri, diags) in by_uri {
            self.client
                .publish_diagnostics(uri.clone(), diags.clone(), None)
                .await;
            self.last_diagnostics.insert(uri, diags.clone());
            flat.extend(diags);
        }
        flat
    }
}

fn collect_roots(docs: &DashMap<Url, DocState>) -> Vec<(Url, WorkspaceFile)> {
    docs.iter()
        .enumerate()
        .map(|(idx, kv)| {
            let doc = kv.value();
            let file = WorkspaceFile {
                path: doc.path.clone(),
                text: doc.text.clone(),
                file_id: gruel_compiler::FileId::new((idx as u32).saturating_add(1).max(1)),
            };
            (kv.key().clone(), file)
        })
        .collect()
}

/// ADR-0092: pick the set of compilation roots to analyze this pass.
///
/// - **Manifested mode**: one root, the manifest's entry file. The entry's
///   text comes from any open buffer at the same path, otherwise from disk.
/// - **Isolation mode**: one root per open buffer, current behaviour.
pub(crate) fn collect_analysis_roots(
    docs: &DashMap<Url, DocState>,
    manifest: Option<&Manifest>,
) -> Vec<(Url, WorkspaceFile)> {
    if let Some(m) = manifest {
        let entry_path = m.target.root().to_path_buf();
        let text = match open_text_lookup(docs, &entry_path) {
            Some(t) => t,
            None => match std::fs::read_to_string(&entry_path) {
                Ok(t) => t,
                Err(_) => {
                    // Entry file disappeared between manifest load and the
                    // first compile — bail out so the user sees no false
                    // diagnostics. The watch handler will refresh once it
                    // reappears.
                    return Vec::new();
                }
            },
        };
        let uri = match Url::from_file_path(&entry_path) {
            Ok(u) => u,
            Err(_) => return Vec::new(),
        };
        let file = WorkspaceFile {
            path: entry_path,
            text,
            file_id: gruel_compiler::FileId::new(1),
        };
        vec![(uri, file)]
    } else {
        collect_roots(docs)
    }
}

/// Compact, `Hash`-able view of an `lsp_types::Range`. `lsp_types::Range`
/// itself doesn't implement `Hash`, so we serialize it to a tuple before
/// using it as a dedup key.
type RangeKey = (u32, u32, u32, u32);

/// Dedup key for diagnostics keyed by URI: (uri, range, message, code).
type UriDiagKey = (Url, RangeKey, String, String);

/// Dedup key for diagnostics keyed by path: (path, range, message, code).
type PathDiagKey = (PathBuf, RangeKey, String, String);

fn range_key(r: &lsp_types::Range) -> RangeKey {
    (r.start.line, r.start.character, r.end.line, r.end.character)
}

fn open_text_lookup(docs: &DashMap<Url, DocState>, path: &std::path::Path) -> Option<String> {
    for kv in docs.iter() {
        if kv.value().path == path {
            return Some(kv.value().text.clone());
        }
    }
    None
}

impl Backend {
    /// ADR-0092: (re-)discover `gruel.json` at `root` and stash the
    /// result in `self.manifest`. Logs (does not raise diagnostics) on
    /// parse / validation failure so the editor user sees what's wrong
    /// without the squiggles spilling into compile diagnostics.
    pub async fn reload_manifest(&self, root: &std::path::Path) {
        let Some(path) = gruel_manifest::discover_at_root(root) else {
            *self.manifest.lock().await = None;
            return;
        };
        match gruel_manifest::load_at(&path) {
            Ok(m) => {
                self.client
                    .log_message(
                        MessageType::INFO,
                        format!("gruel-lsp: loaded manifest at {}", path.display()),
                    )
                    .await;
                *self.manifest.lock().await = Some(m);
            }
            Err(err) => {
                self.client
                    .log_message(
                        MessageType::WARNING,
                        format!("gruel-lsp: invalid manifest at {}: {}", path.display(), err),
                    )
                    .await;
                *self.manifest.lock().await = None;
            }
        }
    }

    /// Return the snapshot for queries against `uri`. If the queried URI is
    /// open, prefer its own per-root snapshot; otherwise (e.g. a file that's
    /// only seen as an `@import` target) fall back to any snapshot whose
    /// closure contains the path. Returns `None` if no snapshot covers the
    /// file yet.
    fn snapshot_for(&self, uri: &Url) -> Option<Arc<Snapshot>> {
        if let Some(snap) = self.snapshots.get(uri) {
            return Some(snap.value().clone());
        }
        let path = uri.to_file_path().ok()?;
        for kv in self.snapshots.iter() {
            if kv.value().path_to_file_id.contains_key(&path) {
                return Some(kv.value().clone());
            }
        }
        None
    }

    /// Return every snapshot, used by workspace-wide queries
    /// (workspace symbols, references). Each entry is unique by root URI.
    fn all_snapshots(&self) -> Vec<Arc<Snapshot>> {
        self.snapshots.iter().map(|kv| kv.value().clone()).collect()
    }
}

struct AnalysisWorker {
    client: Client,
    docs: Arc<DashMap<Url, DocState>>,
    snapshots: Arc<DashMap<Url, Arc<Snapshot>>>,
    preview_features: PreviewFeatures,
    workspace_root: Arc<Mutex<Option<PathBuf>>>,
    /// ADR-0092: same atomic as `Backend::manifest`, watched by every
    /// compile pass so swapping in / out of manifested mode is just a
    /// snapshot read.
    manifest: Arc<Mutex<Option<Manifest>>>,
    /// Currently negotiated position encoding. Used by later phases when
    /// remapping diagnostic ranges through the source text (Phase 1 publishes
    /// byte-based positions, which clients negotiating UTF-8 see correctly).
    #[allow(dead_code)]
    encoding: Arc<Mutex<PositionEncoding>>,
    current_cancel: Arc<Mutex<Option<CancellationToken>>>,
    rx: tokio::sync::mpsc::UnboundedReceiver<AnalysisRequest>,
    target: Target,
    /// Track which files we've previously published diagnostics for so we
    /// can clear stale red squiggles when a file no longer has any.
    published_files: DashMap<Url, ()>,
    /// Shared mirror of the most recent diagnostics per URI (Phase 2).
    last_diagnostics: Arc<DashMap<Url, Vec<Diagnostic>>>,
}

impl AnalysisWorker {
    async fn run(mut self) {
        while let Some(req) = self.rx.recv().await {
            // Coalesce: drain any further pending requests in the queue.
            let debounce = req.debounce;
            let timeout = req.timeout;
            // Sleep until things settle.
            tokio::time::sleep(debounce).await;
            while let Ok(_extra) = self.rx.try_recv() {
                tokio::time::sleep(debounce).await;
            }

            // Cancel any in-flight compile.
            let token = CancellationToken::new();
            {
                let mut cur = self.current_cancel.lock().await;
                if let Some(old) = cur.replace(token.clone()) {
                    old.cancel();
                }
            }

            let workspace_root = self.workspace_root.lock().await.clone();
            let manifest = self.manifest.lock().await.clone();
            let roots = collect_analysis_roots(&self.docs, manifest.as_ref());
            let preview_features = self.preview_features.clone();
            let target = self.target.clone();
            let docs_for_lookup = self.docs.clone();
            let workspace_root_for_task = workspace_root.clone();

            // Run sema on a blocking thread (sema is sync + CPU-heavy).
            //
            // Each open root produces its own `@import` closure; we dedupe
            // diagnostics across roots so a shared imported file with an
            // error doesn't get the same red squiggle published twice.
            let analysis = tokio::task::spawn_blocking(move || {
                let mut snapshots: Vec<(Url, Snapshot)> = Vec::new();
                let mut all_by_file: FxHashMap<PathBuf, Vec<Diagnostic>> = FxHashMap::default();
                let mut seen_keys: FxHashSet<PathDiagKey> = FxHashSet::default();

                for (uri, root) in roots {
                    let result = analysis::analyze_root(
                        root,
                        workspace_root_for_task.as_deref(),
                        &preview_features,
                        &target,
                        |path| open_text_lookup(&docs_for_lookup, path),
                    );
                    if let Some(snap) = result.snapshot {
                        snapshots.push((uri, snap));
                    }
                    let by_file = diagnostics::group_by_file(
                        result.diagnostics.into_iter(),
                        workspace_root_for_task.as_deref(),
                    );
                    for (path, diags) in by_file {
                        for d in diags {
                            let key = (
                                path.clone(),
                                range_key(&d.range),
                                d.message.clone(),
                                d.code
                                    .as_ref()
                                    .map(|c| match c {
                                        lsp_types::NumberOrString::String(s) => s.clone(),
                                        lsp_types::NumberOrString::Number(n) => n.to_string(),
                                    })
                                    .unwrap_or_default(),
                            );
                            if seen_keys.insert(key) {
                                all_by_file.entry(path.clone()).or_default().push(d);
                            }
                        }
                    }
                }
                (snapshots, all_by_file)
            });
            let (snapshots, by_file) = tokio::select! {
                res = analysis => match res {
                    Ok(r) => r,
                    Err(_) => continue,
                },
                _ = tokio::time::sleep(timeout) => {
                    // Timed out — drop result, keep previous snapshots.
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            "gruel-lsp: compile timed out, keeping previous snapshots",
                        )
                        .await;
                    continue;
                }
                _ = token.cancelled() => continue,
            };

            // Drop snapshots whose root URI is neither open nor the
            // current manifest-driven root. In isolation mode that's just
            // open buffers; in manifested mode the entry URI persists
            // even when no editor has it open.
            let mut live_root_uris: FxHashSet<Url> =
                self.docs.iter().map(|kv| kv.key().clone()).collect();
            if let Some(m) = manifest.as_ref()
                && let Ok(u) = Url::from_file_path(m.target.root())
            {
                live_root_uris.insert(u);
            }
            let stale_snapshot_uris: Vec<Url> = self
                .snapshots
                .iter()
                .filter(|kv| !live_root_uris.contains(kv.key()))
                .map(|kv| kv.key().clone())
                .collect();
            for uri in stale_snapshot_uris {
                self.snapshots.remove(&uri);
            }

            // Install the new snapshots.
            for (uri, snap) in snapshots {
                self.snapshots.insert(uri, Arc::new(snap));
            }

            // Clear stale files: any URI we previously published for but
            // doesn't appear in by_file now must be cleared.
            let mut current_files = std::collections::HashSet::new();
            for path in by_file.keys() {
                if let Ok(uri) = Url::from_file_path(path) {
                    current_files.insert(uri);
                }
            }
            let previously_published: Vec<Url> = self
                .published_files
                .iter()
                .map(|kv| kv.key().clone())
                .collect();
            for uri in previously_published {
                if !current_files.contains(&uri) {
                    self.client
                        .publish_diagnostics(uri.clone(), vec![], None)
                        .await;
                    self.published_files.remove(&uri);
                    self.last_diagnostics.remove(&uri);
                }
            }
            // Also clear for any open doc that isn't in by_file (e.g. fixed
            // its errors).
            for kv in self.docs.iter() {
                let uri = kv.key().clone();
                if !current_files.contains(&uri) {
                    self.client
                        .publish_diagnostics(uri.clone(), vec![], None)
                        .await;
                    self.published_files.remove(&uri);
                    self.last_diagnostics.remove(&uri);
                }
            }

            for (path, diags) in by_file {
                if let Ok(uri) = Url::from_file_path(&path) {
                    self.client
                        .publish_diagnostics(uri.clone(), diags.clone(), None)
                        .await;
                    self.published_files.insert(uri.clone(), ());
                    self.last_diagnostics.insert(uri, diags);
                }
            }
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        // Pick UTF-8 if the client supports it; UTF-16 otherwise.
        let chosen_encoding = pick_encoding(&params);
        *self.encoding.lock().await = chosen_encoding;
        let encoding_kind = match chosen_encoding {
            PositionEncoding::Utf8 => PositionEncodingKind::UTF8,
            PositionEncoding::Utf16 => PositionEncodingKind::UTF16,
        };

        // Workspace root: prefer workspaceFolders, then rootUri, then the
        // GRUEL_LSP_ROOT env var (set by `gruel lsp --root` for clients that
        // don't advertise a workspace).
        let root_path = params
            .workspace_folders
            .as_ref()
            .and_then(|fs| fs.first())
            .and_then(|f| f.uri.to_file_path().ok())
            .or_else(|| {
                #[allow(deprecated)]
                params.root_uri.as_ref().and_then(|u| u.to_file_path().ok())
            })
            .or_else(|| std::env::var_os("GRUEL_LSP_ROOT").map(PathBuf::from));

        // ADR-0092: load `gruel.json` at the workspace root, if any.
        // Missing / malformed manifests fall back to isolation mode
        // (the no-manifest default).
        if let Some(root) = root_path.as_deref() {
            self.reload_manifest(root).await;
        }
        *self.workspace_root.lock().await = root_path;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(encoding_kind),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: None,
                    work_done_progress_options: Default::default(),
                }),
                references_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: Some(vec![
                        ".".to_string(),
                        "@".to_string(),
                        ":".to_string(),
                        "(".to_string(),
                    ]),
                    all_commit_characters: None,
                    work_done_progress_options: Default::default(),
                    completion_item: None,
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "gruel-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "gruel-lsp ready")
            .await;
        self.queue_analysis();
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        let state = DocState::new(doc.uri.clone(), doc.text, doc.version, true);
        self.docs.insert(doc.uri, state);
        self.queue_analysis();
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let encoding = *self.encoding.lock().await;
        if let Some(mut entry) = self.docs.get_mut(&params.text_document.uri) {
            for change in params.content_changes {
                if !entry.apply_change(change, encoding) {
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            format!("gruel-lsp: invalid range in {}", entry.uri),
                        )
                        .await;
                }
            }
            entry.version = params.text_document.version;
        }
        self.queue_analysis();
    }

    async fn did_save(&self, _params: DidSaveTextDocumentParams) {
        self.queue_analysis();
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        // Once a doc is closed it stops being a root: drop its dedicated
        // snapshot. The next analysis pass also clears any stale roots, but
        // doing it eagerly keeps queries from racing into a snapshot the
        // editor no longer cares about.
        //
        // In manifested mode (ADR-0092) the per-buffer URI is not the
        // snapshot key, so this removal is a no-op — the manifest-keyed
        // snapshot stays put until the manifest itself goes away.
        self.snapshots.remove(&uri);
        if let Some(mut entry) = self.docs.get_mut(&uri) {
            entry.open = false;
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        // ADR-0092: if any of the watched events touched `gruel.json`,
        // reload the manifest and re-queue analysis. Other watched files
        // are ignored here (we don't currently subscribe to anything
        // else).
        let manifest_changed = params.changes.iter().any(|change| {
            change
                .uri
                .to_file_path()
                .ok()
                .and_then(|p| p.file_name().map(|n| n == "gruel.json"))
                .unwrap_or(false)
        });
        if !manifest_changed {
            return;
        }
        let root = self.workspace_root.lock().await.clone();
        if let Some(root) = root {
            self.reload_manifest(&root).await;
        } else {
            *self.manifest.lock().await = None;
        }
        // Drop snapshots — the compilation unit just changed.
        self.snapshots.clear();
        self.queue_analysis();
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let position = params.text_document_position_params.position;
        let encoding = *self.encoding.lock().await;

        // Resolve the path → file_id via the current snapshot.
        let snap = match self.snapshot_for(&uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let file_id = match snap.path_to_file_id.get(&path) {
            Some(id) => *id,
            None => return Ok(None),
        };
        let source = match snap.sources.get(&file_id) {
            Some(s) => s,
            None => return Ok(None),
        };
        let line_map = match snap.line_maps.get(&file_id) {
            Some(m) => m,
            None => return Ok(None),
        };
        let byte = crate::position::position_to_byte(line_map, &source.text, position, encoding);

        let hover = match crate::hover::hover_at_with_expr_types(
            &snap.ast,
            &snap.interner,
            &snap.expr_types,
            snap.type_pool.as_deref(),
            file_id,
            byte,
        ) {
            Some(h) => h,
            None => return Ok(None),
        };

        let range = crate::position::span_to_range(line_map, &source.text, hover.span, encoding);

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: hover.markdown,
            }),
            range: Some(range),
        }))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let position = params.text_document_position_params.position;
        let encoding = *self.encoding.lock().await;

        let snap = match self.snapshot_for(&uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let file_id = match snap.path_to_file_id.get(&path) {
            Some(id) => *id,
            None => return Ok(None),
        };
        let source = match snap.sources.get(&file_id) {
            Some(s) => s,
            None => return Ok(None),
        };
        let line_map = match snap.line_maps.get(&file_id) {
            Some(m) => m,
            None => return Ok(None),
        };
        let byte = crate::position::position_to_byte(line_map, &source.text, position, encoding);

        let def_span = match crate::goto::definition_at(&snap.ast, &snap.interner, file_id, byte) {
            Some(s) => s,
            None => return Ok(None),
        };

        // Resolve def_span back to (uri, range). It must live in some file we
        // know about.
        let def_file_id = def_span.file_id;
        let def_source = match snap.sources.get(&def_file_id) {
            Some(s) => s,
            None => return Ok(None),
        };
        let def_line_map = match snap.line_maps.get(&def_file_id) {
            Some(m) => m,
            None => return Ok(None),
        };
        let def_uri = match Url::from_file_path(&def_source.path) {
            Ok(u) => u,
            Err(_) => return Ok(None),
        };
        let range =
            crate::position::span_to_range(def_line_map, &def_source.text, def_span, encoding);

        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: def_uri,
            range,
        })))
    }

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> jsonrpc::Result<Option<SignatureHelp>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let position = params.text_document_position_params.position;
        let encoding = *self.encoding.lock().await;

        let snap = match self.snapshot_for(&uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let file_id = match snap.path_to_file_id.get(&path) {
            Some(id) => *id,
            None => return Ok(None),
        };
        let source = match snap.sources.get(&file_id) {
            Some(s) => s,
            None => return Ok(None),
        };
        let line_map = match snap.line_maps.get(&file_id) {
            Some(m) => m,
            None => return Ok(None),
        };
        let byte = crate::position::position_to_byte(line_map, &source.text, position, encoding);

        let result =
            match crate::signature::signature_help(&snap.ast, &snap.interner, file_id, byte) {
                Some(r) => r,
                None => return Ok(None),
            };

        let parameters = result
            .parameters
            .iter()
            .map(|(s, e)| ParameterInformation {
                label: ParameterLabel::LabelOffsets([*s, *e]),
                documentation: None,
            })
            .collect();

        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: result.label,
                documentation: None,
                parameters: Some(parameters),
                active_parameter: Some(result.active_parameter as u32),
            }],
            active_signature: Some(0),
            active_parameter: Some(result.active_parameter as u32),
        }))
    }

    async fn references(&self, params: ReferenceParams) -> jsonrpc::Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let position = params.text_document_position.position;
        let include_decl = params.context.include_declaration;
        let encoding = *self.encoding.lock().await;

        let target_path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };

        // References can span multiple compilation units (e.g. a function in
        // a shared `utils.gruel` is called by two different open roots). Walk
        // every snapshot whose closure contains the target file, run the
        // per-snapshot query, and union the results, deduping by location.
        let mut locations = Vec::new();
        let mut seen: FxHashSet<(Url, RangeKey)> = FxHashSet::default();
        for snap in self.all_snapshots() {
            let Some(&file_id) = snap.path_to_file_id.get(&target_path) else {
                continue;
            };
            let Some(source) = snap.sources.get(&file_id) else {
                continue;
            };
            let Some(line_map) = snap.line_maps.get(&file_id) else {
                continue;
            };
            let byte =
                crate::position::position_to_byte(line_map, &source.text, position, encoding);
            let spans = crate::references::references_at(
                &snap.ast,
                &snap.interner,
                file_id,
                byte,
                include_decl,
            );
            for s in spans {
                let Some(src) = snap.sources.get(&s.file_id) else {
                    continue;
                };
                let Some(lm) = snap.line_maps.get(&s.file_id) else {
                    continue;
                };
                let Ok(loc_uri) = Url::from_file_path(&src.path) else {
                    continue;
                };
                let range = crate::position::span_to_range(lm, &src.text, s, encoding);
                if seen.insert((loc_uri.clone(), range_key(&range))) {
                    locations.push(Location {
                        uri: loc_uri,
                        range,
                    });
                }
            }
        }

        if locations.is_empty() {
            Ok(None)
        } else {
            Ok(Some(locations))
        }
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> jsonrpc::Result<Option<Vec<SymbolInformation>>> {
        let encoding = *self.encoding.lock().await;
        // Workspace symbols span every open root: walk each per-root snapshot,
        // dedupe by definition location so a function imported by two
        // different roots doesn't get listed twice.
        let mut out: Vec<SymbolInformation> = Vec::new();
        let mut seen: FxHashSet<(Url, RangeKey, String)> = FxHashSet::default();
        for snap in self.all_snapshots() {
            let syms = crate::workspace_symbols::workspace_symbols(
                &snap.ast,
                &snap.interner,
                &params.query,
            );
            for sym in syms {
                let src = match snap.sources.get(&sym.span.file_id) {
                    Some(x) => x,
                    None => continue,
                };
                let lm = match snap.line_maps.get(&sym.span.file_id) {
                    Some(m) => m,
                    None => continue,
                };
                let uri = match Url::from_file_path(&src.path) {
                    Ok(u) => u,
                    Err(_) => continue,
                };
                let range = crate::position::span_to_range(lm, &src.text, sym.span, encoding);
                if !seen.insert((uri.clone(), range_key(&range), sym.name.clone())) {
                    continue;
                }
                #[allow(deprecated)]
                out.push(SymbolInformation {
                    name: sym.name,
                    kind: to_lsp_kind(sym.kind),
                    tags: None,
                    deprecated: None,
                    location: Location { uri, range },
                    container_name: sym.container,
                });
            }
        }
        if out.is_empty() {
            Ok(None)
        } else {
            Ok(Some(out))
        }
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let position = params.text_document_position.position;
        let encoding = *self.encoding.lock().await;
        let trigger = params
            .context
            .as_ref()
            .and_then(|c| c.trigger_character.as_ref())
            .and_then(|s| s.chars().next());

        let snap = match self.snapshot_for(&uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let file_id = match snap.path_to_file_id.get(&path) {
            Some(id) => *id,
            None => return Ok(None),
        };
        let source = match snap.sources.get(&file_id) {
            Some(s) => s,
            None => return Ok(None),
        };
        let line_map = match snap.line_maps.get(&file_id) {
            Some(m) => m,
            None => return Ok(None),
        };
        let byte = crate::position::position_to_byte(line_map, &source.text, position, encoding);

        let items =
            crate::completion::complete_at(&snap.ast, &snap.interner, file_id, byte, trigger);
        let lsp_items: Vec<CompletionItem> = items
            .into_iter()
            .map(|i| CompletionItem {
                label: i.label,
                kind: Some(to_completion_kind(i.kind)),
                detail: i.detail,
                ..CompletionItem::default()
            })
            .collect();
        if lsp_items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(CompletionResponse::Array(lsp_items)))
        }
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> jsonrpc::Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri.clone();
        let encoding = *self.encoding.lock().await;

        let snap = match self.snapshot_for(&uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let file_id = match snap.path_to_file_id.get(&path) {
            Some(id) => *id,
            None => return Ok(None),
        };
        let source = match snap.sources.get(&file_id) {
            Some(s) => s,
            None => return Ok(None),
        };
        let line_map = match snap.line_maps.get(&file_id) {
            Some(m) => m,
            None => return Ok(None),
        };

        let hints = crate::inlay_hints::inlay_hints(
            &snap.ast,
            &snap.interner,
            &snap.expr_types,
            snap.type_pool.as_deref(),
            file_id,
        );

        let lsp_hints: Vec<InlayHint> = hints
            .into_iter()
            .filter(|h| h.file_id == file_id)
            .map(|h| {
                let pos =
                    crate::position::byte_to_position(line_map, &source.text, h.byte, encoding);
                InlayHint {
                    position: pos,
                    label: InlayHintLabel::String(h.label),
                    kind: Some(match h.kind {
                        crate::inlay_hints::InlayKind::Type => InlayHintKind::TYPE,
                        crate::inlay_hints::InlayKind::Parameter => InlayHintKind::PARAMETER,
                    }),
                    text_edits: None,
                    tooltip: None,
                    padding_left: None,
                    padding_right: None,
                    data: None,
                }
            })
            .collect();
        if lsp_hints.is_empty() {
            Ok(None)
        } else {
            Ok(Some(lsp_hints))
        }
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri.clone();

        // Look up the in-memory buffer. If absent (file the editor doesn't
        // have open), there's nothing to format.
        let original = match self.docs.get(&uri) {
            Some(doc) => doc.text.clone(),
            None => return Ok(None),
        };

        // Run the formatter. Parse failure returns Ok(None) so the editor
        // leaves the buffer alone — diagnostics from the existing pipeline
        // already cover the cause (ADR-0093 §LSP integration).
        let formatted = match gruel_fmt::format_source(&original) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(uri = %uri, error = %e, "gruel-fmt: parse failed");
                return Ok(None);
            }
        };

        if formatted == original {
            // Already canonical — return an empty edit list so the editor
            // records a clean save.
            return Ok(Some(Vec::new()));
        }

        Ok(Some(diff_to_text_edits(&original, &formatted)))
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri.clone();
        let range = params.range;
        let encoding = *self.encoding.lock().await;
        let workspace_root = self.workspace_root.lock().await.clone();
        let diagnostics = self
            .last_diagnostics
            .get(&uri)
            .map(|d| d.clone())
            .unwrap_or_default();
        let actions: Vec<CodeActionOrCommand> = crate::code_actions::code_actions_for_range(
            &diagnostics,
            range,
            &self.docs,
            encoding,
            workspace_root.as_deref(),
        );
        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }
}

/// Convert (original, formatted) into a minimal list of `TextEdit`s, one per
/// change hunk (ADR-0093 Phase 7).
///
/// Hunks are line-aligned: each edit replaces a contiguous range of lines
/// with the corresponding lines from `formatted`. Editors keep cursor /
/// fold state on the untouched lines, which a single whole-document replace
/// would clobber. Column 0 is encoding-agnostic, so this works under either
/// UTF-8 or UTF-16 negotiation without going through the `LineMap`.
fn diff_to_text_edits(original: &str, formatted: &str) -> Vec<TextEdit> {
    use similar::{DiffOp, TextDiff};

    let diff = TextDiff::from_lines(original, formatted);
    let new_lines: Vec<&str> = formatted.split_inclusive('\n').collect();

    let mut edits = Vec::new();
    // grouped_ops(0): split on Equal runs of any length, so each group is a
    // contiguous run of changes.
    for group in diff.grouped_ops(0) {
        if group.is_empty() {
            continue;
        }
        // Skip pure-Equal groups (no changes).
        let all_equal = group.iter().all(|op| matches!(op, DiffOp::Equal { .. }));
        if all_equal {
            continue;
        }

        let mut old_start = usize::MAX;
        let mut old_end = 0;
        let mut new_start = usize::MAX;
        let mut new_end = 0;
        for op in &group {
            let (os, oe) = (op.old_range().start, op.old_range().end);
            let (ns, ne) = (op.new_range().start, op.new_range().end);
            old_start = old_start.min(os);
            old_end = old_end.max(oe);
            new_start = new_start.min(ns);
            new_end = new_end.max(ne);
        }

        // Collect the new lines for this hunk back into a single replacement
        // string. `split_inclusive` keeps newlines, so concatenation
        // reconstructs the body byte-for-byte.
        let new_text: String = new_lines[new_start..new_end].concat();

        let range = Range {
            start: Position {
                line: old_start as u32,
                character: 0,
            },
            end: Position {
                line: old_end as u32,
                character: 0,
            },
        };
        edits.push(TextEdit { range, new_text });
    }
    edits
}

fn pick_encoding(params: &InitializeParams) -> PositionEncoding {
    if let Some(general) = params.capabilities.general.as_ref() {
        if let Some(encs) = general.position_encodings.as_ref() {
            for e in encs {
                if *e == PositionEncodingKind::UTF8 {
                    return PositionEncoding::Utf8;
                }
            }
        }
    }
    PositionEncoding::Utf16
}

/// Run the LSP server reading from stdin and writing to stdout.
pub async fn run_stdio_server(preview_features: PreviewFeatures) -> std::io::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) =
        LspService::new(move |client| Backend::new(client, preview_features.clone()));
    Server::new(stdin, stdout, socket).serve(service).await;
    Ok(())
}

fn to_lsp_kind(k: crate::workspace_symbols::SymbolKind) -> LspSymbolKind {
    use crate::workspace_symbols::SymbolKind as K;
    match k {
        K::Function => LspSymbolKind::FUNCTION,
        K::Struct => LspSymbolKind::STRUCT,
        K::Enum => LspSymbolKind::ENUM,
        K::Interface => LspSymbolKind::INTERFACE,
        K::Derive => LspSymbolKind::CLASS,
        K::Constant => LspSymbolKind::CONSTANT,
        K::Field => LspSymbolKind::FIELD,
        K::EnumMember => LspSymbolKind::ENUM_MEMBER,
        K::Method => LspSymbolKind::METHOD,
    }
}

fn to_completion_kind(k: crate::completion::CompletionKind) -> CompletionItemKind {
    use crate::completion::CompletionKind as K;
    match k {
        K::Function => CompletionItemKind::FUNCTION,
        K::Struct => CompletionItemKind::STRUCT,
        K::Enum => CompletionItemKind::ENUM,
        K::Interface => CompletionItemKind::INTERFACE,
        K::Derive => CompletionItemKind::CLASS,
        K::Constant => CompletionItemKind::CONSTANT,
        K::Field => CompletionItemKind::FIELD,
        K::EnumMember => CompletionItemKind::ENUM_MEMBER,
        K::Variable => CompletionItemKind::VARIABLE,
        K::Method => CompletionItemKind::METHOD,
        K::Keyword => CompletionItemKind::KEYWORD,
        K::Intrinsic => CompletionItemKind::FUNCTION,
    }
}

/// Synchronous entry point used by the binary subcommand (creates a tokio
/// runtime and starts the stdio server).
pub fn run_server(preview_features: PreviewFeatures) -> std::io::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(run_stdio_server(preview_features))
}
