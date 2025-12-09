use crate::filesystem::collect_files_with_ext;
use crate::indexer::Indexer;
use crate::lang::{LanguageService, java::JavaService};
use crate::library::SourceArchiveRegistry;
use crate::state::{Document, GlobalIndex};
use dashmap::DashMap;
use ropey::Rope;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tree_sitter::{InputEdit, Point};
use zip::ZipArchive;

#[derive(Clone)]
pub struct ServerConfig {
    pub keywords: Vec<String>,
}

pub struct LspBackend {
    pub client: Client,
    pub documents: DashMap<String, Document>,
    pub index: Arc<GlobalIndex>,
    services: HashMap<String, Box<dyn LanguageService>>,
    parsers: DashMap<String, Mutex<tree_sitter::Parser>>,
    workspace_root: RwLock<Option<PathBuf>>,
    source_archives: Arc<SourceArchiveRegistry>,
    config: ServerConfig,
}

impl LspBackend {
    pub fn new(client: Client, config: ServerConfig) -> Self {
        let mut services: HashMap<String, Box<dyn LanguageService>> = HashMap::new();

        // TODO: register kotlin service, gradle service
        services.insert("java".to_string(), Box::new(JavaService));

        let parsers = DashMap::new();
        for (ext, service) in &services {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&service.language())
                .expect("Failed to load language");
            parsers.insert(ext.clone(), Mutex::new(parser));
        }

        Self {
            client,
            documents: DashMap::new(),
            index: Arc::new(GlobalIndex::new()),
            services,
            parsers,
            workspace_root: RwLock::new(None),
            source_archives: Arc::new(SourceArchiveRegistry::new()),
            config,
        }
    }

    fn get_ext(&self, uri: &str) -> Option<String> {
        uri.split('.').next_back().map(|s| s.to_string())
    }

    async fn index_workspace(&self) {
        let root = match self.workspace_root.read() {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        };

        let Some(root) = root else {
            tracing::info!("Skip workspace indexing: no root provided by client");
            return;
        };

        let java_files =
            match tokio::task::spawn_blocking(move || collect_files_with_ext(root, "java")).await {
                Ok(list) => list,
                Err(err) => {
                    tracing::error!("Failed to collect files for indexing: {err}");
                    return;
                }
            };

        if java_files.is_empty() {
            tracing::info!("No Java files found during workspace indexing");
            return;
        }

        tracing::info!("Indexing {} Java files...", java_files.len());
        for path in java_files {
            if let Err(err) = self.index_single_file(&path).await {
                tracing::warn!("Indexing failed for {:?}: {}", path, err);
            }
        }
        tracing::info!("Workspace indexing finished");
    }

    async fn index_single_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("Missing extension"))?;
        if !self.services.contains_key(ext) {
            return Ok(());
        }

        let uri = Url::from_file_path(path)
            .map_err(|_| anyhow::anyhow!("Invalid file path for URL: {:?}", path))?;
        let text = std::fs::read_to_string(path)?;
        let rope = Rope::from_str(&text);

        let parser_lock = self
            .parsers
            .get(ext)
            .ok_or_else(|| anyhow::anyhow!("No parser registered for extension '{}'", ext))?;
        let mut parser = parser_lock.lock().await;
        let tree = parser
            .parse_with_options(
                &mut |offset, _| rope.byte_slice(offset..).chunks().next().unwrap_or(""),
                None,
                None,
            )
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file {:?}", path))?;

        Indexer::update_file(&self.index, &uri.to_string(), &tree, &rope);
        Ok(())
    }

    async fn index_builtin_library(&self) {
        let java_home = match std::env::var("JAVA_HOME") {
            Ok(val) => PathBuf::from(val),
            Err(_) => {
                tracing::info!("JAVA_HOME not set; skip JDK source indexing");
                return;
            }
        };

        let mut candidates = vec![
            java_home.join("lib").join("src.zip"),
            java_home.join("src.zip"),
        ];
        candidates.retain(|p| p.exists());

        let Some(zip_path) = candidates.into_iter().next() else {
            tracing::info!("No src.zip found in JAVA_HOME; skip JDK source indexing");
            return;
        };

        tracing::info!("Indexing JDK sources from {:?}", zip_path);
        self.source_archives
            .register_zip("jrsls-std", zip_path.clone());
        let index = self.index.clone();

        let result = tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(&zip_path)?;
            let mut archive = ZipArchive::new(file)?;

            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_java::LANGUAGE.into())
                .map_err(|e| anyhow::anyhow!("Failed to load Java grammar: {}", e))?;

            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)?;
                if entry.is_dir() {
                    continue;
                }
                let name = entry.name().to_string();
                if !name.ends_with(".java") {
                    continue;
                }

                let mut contents = String::new();
                use std::io::Read;
                entry.read_to_string(&mut contents)?;

                let rope = Rope::from_str(&contents);
                let tree = parser
                    .parse_with_options(
                        &mut |offset, _| rope.byte_slice(offset..).chunks().next().unwrap_or(""),
                        None,
                        None,
                    )
                    .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", name))?;

                let uri = format!("jrsls-std:///{}", name);
                Indexer::update_file(&index, &uri, &tree, &rope);
            }

            anyhow::Ok(())
        })
        .await;

        match result {
            Ok(Ok(_)) => tracing::info!("JDK source indexing finished"),
            Ok(Err(err)) => tracing::warn!("JDK source indexing failed: {}", err),
            Err(err) => tracing::warn!("JDK source indexing task panicked: {}", err),
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for LspBackend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        if let Some(root) = params
            .root_uri
            .and_then(|u| u.to_file_path().ok())
            .or_else(|| {
                params
                    .workspace_folders
                    .as_ref()
                    .and_then(|folders| folders.first())
                    .and_then(|folder| folder.uri.to_file_path().ok())
            })
        {
            if let Ok(mut guard) = self.workspace_root.write() {
                *guard = Some(root);
            }
        }

        tracing::info!("Lsp Initialzed");
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                definition_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Server initialized!")
            .await;
        self.index_workspace().await;
        self.index_builtin_library().await;
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let ext = match self.get_ext(&uri) {
            Some(e) => e,
            None => return,
        };

        if !self.parsers.contains_key(&ext) {
            return;
        }

        let text = params.text_document.text;
        let rope = Rope::from_str(&text);

        let parser = self.parsers.get(&ext).unwrap();
        let mut parser = parser.lock().await;
        let tree = parser
            .parse_with_options(
                &mut |offset, _| rope.byte_slice(offset..).chunks().next().unwrap_or(""),
                None,
                None,
            )
            .unwrap();

        tracing::info!("Parsed file {}", uri);
        self.documents
            .insert(uri.clone(), Document { text: rope, tree });
        if let Some(doc) = self.documents.get(&uri) {
            Indexer::update_file(&self.index, &uri, &doc.tree, &doc.text);
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let ext = match self.get_ext(&uri) {
            Some(e) => e,
            None => return,
        };

        if let Some(mut doc) = self.documents.get_mut(&uri) {
            let parser_lock = self.parsers.get(&ext);
            if parser_lock.is_none() {
                return;
            }

            let parser = parser_lock.unwrap();
            let mut parser = parser.lock().await;

            for change in params.content_changes {
                if change.range.is_none() {
                    let rope = Rope::from_str(&change.text);
                    let tree = parser
                        .parse_with_options(
                            &mut |offset, _| {
                                rope.byte_slice(offset..).chunks().next().unwrap_or("")
                            },
                            None,
                            None,
                        )
                        .unwrap();
                    doc.text = rope;
                    doc.tree = tree;
                    continue;
                }

                let range = change.range.unwrap();
                let start_line = range.start.line as usize;
                let end_line = range.end.line as usize;

                // 防御性检查
                if start_line >= doc.text.len_lines() || end_line >= doc.text.len_lines() {
                    continue;
                }

                let start_char_idx =
                    doc.text.line_to_char(start_line) + range.start.character as usize;
                let end_char_idx = doc.text.line_to_char(end_line) + range.end.character as usize;
                let start_byte = doc.text.char_to_byte(start_char_idx);
                let old_end_byte = doc.text.char_to_byte(end_char_idx);

                doc.text.remove(start_char_idx..end_char_idx);
                doc.text.insert(start_char_idx, &change.text);

                let new_end_byte = start_byte + change.text.len();
                let new_end_char_idx = doc.text.byte_to_char(new_end_byte);
                let new_end_line = doc.text.char_to_line(new_end_char_idx);
                let new_end_col = new_end_char_idx - doc.text.line_to_char(new_end_line);

                let edit = InputEdit {
                    start_byte,
                    old_end_byte,
                    new_end_byte,
                    start_position: Point::new(start_line, range.start.character as usize),
                    old_end_position: Point::new(end_line, range.end.character as usize),
                    new_end_position: Point::new(new_end_line, new_end_col),
                };

                doc.tree.edit(&edit);
            }

            let rope = &doc.text;
            let new_tree = parser
                .parse_with_options(
                    &mut |offset, _| rope.byte_slice(offset..).chunks().next().unwrap_or(""),
                    Some(&doc.tree),
                    None,
                )
                .unwrap();

            doc.tree = new_tree;

            // update global index
            Indexer::update_file(&self.index, &uri, &doc.tree, &doc.text);
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri.to_string();
        let ext = match self.get_ext(&uri) {
            Some(e) => e,
            None => return Ok(None),
        };

        if let Some(doc) = self.documents.get(&uri) {
            // 根据后缀分发给对应的 Service (Java/Kotlin)
            if let Some(service) = self.services.get(&ext) {
                let symbols = service.document_symbol(&doc.tree, &doc.text);
                return Ok(Some(DocumentSymbolResponse::Nested(symbols)));
            }
        }

        Ok(None)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .to_string();
        let position = params.text_document_position_params.position;
        let ext = match self.get_ext(&uri) {
            Some(e) => e,
            None => return Ok(None),
        };

        if let Some(doc) = self.documents.get(&uri)
            && let Some(service) = self.services.get(&ext)
            && let Some(mut location) =
                service.goto_definition(&doc.tree, &doc.text, position, &self.index, &uri)
        {
            if let Some(materialized) = self.source_archives.materialize(&location) {
                location = materialized;
            }
            return Ok(Some(GotoDefinitionResponse::Scalar(location)));
        }

        Ok(None)
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri.to_string();
        let position = params.text_document_position.position;
        let ext = match self.get_ext(&uri) {
            Some(e) => e,
            None => return Ok(None),
        };

        if let Some(doc) = self.documents.get(&uri)
            && let Some(service) = self.services.get(&ext)
        {
            if let Some(items) = service.completion(
                &doc.tree,
                &doc.text,
                position,
                &self.index,
                &uri,
                &self.config.keywords,
            ) {
                return Ok(Some(CompletionResponse::Array(items)));
            }
        }

        Ok(None)
    }
}
