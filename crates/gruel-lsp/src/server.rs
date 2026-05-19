//! `tower-lsp` `LanguageServer` impl (ADR-0091).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use gruel_compiler::{FileId, PreviewFeatures};
use gruel_target::Target;
use lsp_types::{
    CodeActionOrCommand, CodeActionParams, CodeActionProviderCapability, CodeActionResponse,
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, HoverProviderCapability,
    InitializeParams, InitializeResult, InitializedParams, InlayHint, InlayHintKind,
    InlayHintLabel, InlayHintParams, Location, MarkupContent, MarkupKind, MessageType, OneOf,
    ParameterInformation, ParameterLabel, PositionEncodingKind, ReferenceParams,
    ServerCapabilities, ServerInfo, SignatureHelp, SignatureHelpOptions, SignatureHelpParams,
    SignatureInformation, SymbolInformation, SymbolKind as LspSymbolKind,
    TextDocumentSyncCapability, TextDocumentSyncKind, Url, WorkspaceSymbolParams,
};
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;
use tower_lsp::{Client, LanguageServer, LspService, Server, jsonrpc};

use crate::analysis::{self, Snapshot, WorkspaceFile};
use crate::diagnostics;
use crate::document::DocState;
use crate::position::PositionEncoding;
use crate::workspace::enumerate_gruel_files;

/// Default debounce duration between the last keystroke and the next compile.
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(150);
/// Hard upper bound on a single compile pass.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// LSP backend state shared by every request handler.
pub struct Backend {
    pub client: Client,
    pub docs: Arc<DashMap<Url, DocState>>,
    pub snapshot: Arc<ArcSwap<Option<Snapshot>>>,
    pub preview_features: PreviewFeatures,
    pub workspace_root: Arc<Mutex<Option<PathBuf>>>,
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
            snapshot: Arc::new(ArcSwap::from_pointee(None)),
            preview_features,
            workspace_root: Arc::new(Mutex::new(None)),
            encoding: Arc::new(Mutex::new(PositionEncoding::Utf16)),
            analysis_tx: tx,
            current_cancel: Arc::new(Mutex::new(None)),
            last_diagnostics: Arc::new(DashMap::new()),
        };
        // Spawn the analysis worker.
        let worker = AnalysisWorker {
            client,
            docs: me.docs.clone(),
            snapshot: me.snapshot.clone(),
            preview_features: me.preview_features.clone(),
            workspace_root: me.workspace_root.clone(),
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

    /// Test-only: compile the workspace synchronously and publish diagnostics.
    /// Bypasses the debounce / spawned worker so integration tests can poll a
    /// deterministic result.
    pub async fn analyze_now(&self) -> Vec<Diagnostic> {
        let files = gather_workspace_files(
            &self.docs,
            self.workspace_root.lock().await.clone().as_deref(),
        );
        let result = analysis::analyze(&files, &self.preview_features, &Target::host());
        let root = self.workspace_root.lock().await.clone();
        let by_file =
            diagnostics::group_by_file(result.diagnostics.into_iter(), root.as_deref());
        let mut flat = Vec::new();
        for (path, diags) in &by_file {
            if let Ok(uri) = Url::from_file_path(path) {
                self.client
                    .publish_diagnostics(uri.clone(), diags.clone(), None)
                    .await;
                self.last_diagnostics.insert(uri, diags.clone());
            }
            flat.extend(diags.clone());
        }
        if let Some(snap) = result.snapshot {
            self.snapshot.store(Arc::new(Some(snap)));
        }
        flat
    }
}

struct AnalysisWorker {
    client: Client,
    docs: Arc<DashMap<Url, DocState>>,
    snapshot: Arc<ArcSwap<Option<Snapshot>>>,
    preview_features: PreviewFeatures,
    workspace_root: Arc<Mutex<Option<PathBuf>>>,
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

            let files =
                gather_workspace_files(&self.docs, self.workspace_root.lock().await.clone().as_deref());
            let preview_features = self.preview_features.clone();
            let target = self.target.clone();

            // Run sema on a blocking thread (sema is sync + CPU-heavy).
            let analysis = tokio::task::spawn_blocking(move || {
                analysis::analyze(&files, &preview_features, &target)
            });
            let result = tokio::select! {
                res = analysis => match res {
                    Ok(r) => r,
                    Err(_) => continue,
                },
                _ = tokio::time::sleep(timeout) => {
                    // Timed out — drop result, keep previous snapshot.
                    self.client
                        .log_message(
                            MessageType::WARNING,
                            "gruel-lsp: compile timed out, keeping previous snapshot",
                        )
                        .await;
                    continue;
                }
                _ = token.cancelled() => continue,
            };

            // Publish diagnostics.
            let root = self.workspace_root.lock().await.clone();
            let by_file =
                diagnostics::group_by_file(result.diagnostics.into_iter(), root.as_deref());

            // Clear stale files: any URI we previously published for but
            // doesn't appear in by_file now must be cleared.
            let mut current_files = std::collections::HashSet::new();
            for path in by_file.keys() {
                if let Ok(uri) = Url::from_file_path(path) {
                    current_files.insert(uri);
                }
            }
            // Snapshot the published_files set for safe iteration.
            let previously_published: Vec<Url> = self
                .published_files
                .iter()
                .map(|kv| kv.key().clone())
                .collect();
            for uri in previously_published {
                if !current_files.contains(&uri) {
                    self.client.publish_diagnostics(uri.clone(), vec![], None).await;
                    self.published_files.remove(&uri);
                    self.last_diagnostics.remove(&uri);
                }
            }
            // Also clear for any open doc that isn't in by_file (e.g. fixed
            // its errors).
            for kv in self.docs.iter() {
                let uri = kv.key().clone();
                if !current_files.contains(&uri) {
                    self.client.publish_diagnostics(uri.clone(), vec![], None).await;
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

            if let Some(snap) = result.snapshot {
                self.snapshot.store(Arc::new(Some(snap)));
            }
        }
    }
}

fn gather_workspace_files(
    docs: &DashMap<Url, DocState>,
    workspace_root: Option<&std::path::Path>,
) -> Vec<WorkspaceFile> {
    let mut files = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();
    let mut next_id: u32 = 1;

    // Open buffers take precedence.
    for kv in docs.iter() {
        let doc = kv.value();
        files.push(WorkspaceFile {
            path: doc.path.clone(),
            text: doc.text.clone(),
            file_id: FileId::new(next_id),
        });
        seen_paths.insert(doc.path.clone());
        next_id += 1;
    }

    // Fall back to disk for files we haven't been notified about.
    if let Some(root) = workspace_root {
        for path in enumerate_gruel_files(root) {
            if seen_paths.contains(&path) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                files.push(WorkspaceFile {
                    path: path.clone(),
                    text,
                    file_id: FileId::new(next_id),
                });
                next_id += 1;
            }
        }
    }

    files
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> jsonrpc::Result<InitializeResult> {
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
                params
                    .root_uri
                    .as_ref()
                    .and_then(|u| u.to_file_path().ok())
            })
            .or_else(|| std::env::var_os("GRUEL_LSP_ROOT").map(PathBuf::from));
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
        // Keep state but mark closed.
        if let Some(mut entry) = self.docs.get_mut(&params.text_document.uri) {
            entry.open = false;
        }
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri.clone();
        let position = params.text_document_position_params.position;
        let encoding = *self.encoding.lock().await;

        // Resolve the path → file_id via the current snapshot.
        let snap_guard = self.snapshot.load();
        let snap = match snap_guard.as_ref().as_ref() {
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
        let uri = params.text_document_position_params.text_document.uri.clone();
        let position = params.text_document_position_params.position;
        let encoding = *self.encoding.lock().await;

        let snap_guard = self.snapshot.load();
        let snap = match snap_guard.as_ref().as_ref() {
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
        let range = crate::position::span_to_range(
            def_line_map,
            &def_source.text,
            def_span,
            encoding,
        );

        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: def_uri,
            range,
        })))
    }

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> jsonrpc::Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri.clone();
        let position = params.text_document_position_params.position;
        let encoding = *self.encoding.lock().await;

        let snap_guard = self.snapshot.load();
        let snap = match snap_guard.as_ref().as_ref() {
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

        let result = match crate::signature::signature_help(
            &snap.ast,
            &snap.interner,
            file_id,
            byte,
        ) {
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

    async fn references(
        &self,
        params: ReferenceParams,
    ) -> jsonrpc::Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let position = params.text_document_position.position;
        let include_decl = params.context.include_declaration;
        let encoding = *self.encoding.lock().await;

        let snap_guard = self.snapshot.load();
        let snap = match snap_guard.as_ref().as_ref() {
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

        let spans =
            crate::references::references_at(&snap.ast, &snap.interner, file_id, byte, include_decl);

        let mut locations = Vec::new();
        for s in spans {
            let src = match snap.sources.get(&s.file_id) {
                Some(x) => x,
                None => continue,
            };
            let lm = match snap.line_maps.get(&s.file_id) {
                Some(m) => m,
                None => continue,
            };
            let uri = match Url::from_file_path(&src.path) {
                Ok(u) => u,
                Err(_) => continue,
            };
            let range = crate::position::span_to_range(lm, &src.text, s, encoding);
            locations.push(Location { uri, range });
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
        let snap_guard = self.snapshot.load();
        let snap = match snap_guard.as_ref().as_ref() {
            Some(s) => s,
            None => return Ok(None),
        };
        let syms = crate::workspace_symbols::workspace_symbols(
            &snap.ast,
            &snap.interner,
            &params.query,
        );
        let mut out: Vec<SymbolInformation> = Vec::new();
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

        let snap_guard = self.snapshot.load();
        let snap = match snap_guard.as_ref().as_ref() {
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

        let items = crate::completion::complete_at(&snap.ast, &snap.interner, file_id, byte, trigger);
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

    async fn inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> jsonrpc::Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri.clone();
        let encoding = *self.encoding.lock().await;

        let snap_guard = self.snapshot.load();
        let snap = match snap_guard.as_ref().as_ref() {
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
                let pos = crate::position::byte_to_position(line_map, &source.text, h.byte, encoding);
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
