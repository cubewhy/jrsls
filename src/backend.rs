use crate::lang::{LanguageService, java::JavaService};
use crate::state::Document;
use dashmap::DashMap;
use ropey::Rope;
use std::collections::HashMap;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tree_sitter::{InputEdit, Point};

pub struct LspBackend {
    pub client: Client,
    pub documents: DashMap<String, Document>,
    services: HashMap<String, Box<dyn LanguageService>>,
    parsers: DashMap<String, Mutex<tree_sitter::Parser>>,
}

impl LspBackend {
    pub fn new(client: Client) -> Self {
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
            services,
            parsers,
        }
    }

    fn get_ext(&self, uri: &str) -> Option<String> {
        uri.split('.').last().map(|s| s.to_string())
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for LspBackend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        tracing::info!("Lsp Initialzed");
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                document_symbol_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions::default()),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Server initialized!")
            .await;
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
        self.documents.insert(uri, Document { text: rope, tree });
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

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
