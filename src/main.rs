use dashmap::DashMap;
use ropey::Rope;
use tokio::sync::Mutex;
use tower_lsp::{
    Client, LanguageServer, LspService, Server, jsonrpc,
    lsp_types::{
        self, CompletionOptions, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
        DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, InitializeParams,
        InitializeResult, InitializedParams, MessageType, OneOf, Position, ServerCapabilities,
        SymbolKind, TextDocumentSyncCapability, TextDocumentSyncKind,
    },
};
use tree_sitter::{InputEdit, Point};

struct Document {
    text: Rope,
    tree: tree_sitter::Tree,
}

struct LspBackend {
    client: Client,
    parser: Mutex<tree_sitter::Parser>,
    documents: DashMap<String, Document>,
}

impl LspBackend {
    pub fn new(client: Client) -> Self {
        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_java::LANGUAGE;
        parser
            .set_language(&language.into())
            .expect("Failed to set java parser");

        Self {
            client,
            parser: parser.into(),
            documents: DashMap::new(),
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for LspBackend {
    async fn initialize(&self, _: InitializeParams) -> jsonrpc::Result<InitializeResult> {
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
            .log_message(MessageType::INFO, "server initialized!")
            .await;
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.to_string();
        let text = params.text_document.text;

        let rope = Rope::from_str(&text);

        let mut parser = self.parser.lock().await;

        let tree = parser
            .parse_with_options(
                &mut |offset, _position| rope.byte_slice(offset..).chunks().next().unwrap_or(""),
                None,
                None,
            )
            .unwrap();

        tracing::info!("Parsed file {uri}");

        self.documents.insert(uri, Document { text: rope, tree });
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.to_string();

        if let Some(mut doc) = self.documents.get_mut(&uri) {
            let mut parser = self.parser.lock().await;

            for change in params.content_changes {
                // 1. 处理全量替换 (Full Sync)
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

                let max_line = doc.text.len_lines();
                if start_line >= max_line || end_line >= max_line {
                    tracing::error!(
                        "CRITICAL: Client tried to edit line {}, but I only have {} lines. Skipping edit to prevent crash.",
                        start_line,
                        max_line
                    );
                    continue;
                }

                let start_char_idx =
                    doc.text.line_to_char(start_line) + range.start.character as usize;
                let end_char_idx = doc.text.line_to_char(end_line) + range.end.character as usize;

                if end_char_idx > doc.text.len_chars() {
                    tracing::error!("CRITICAL: Char index out of bounds.");
                    continue;
                }

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
    ) -> jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri.to_string();

        if let Some(doc) = self.documents.get(&uri) {
            let tree = &doc.tree;
            let text = &doc.text;

            let symbols = traverse_node(tree.root_node(), text);

            return Ok(Some(DocumentSymbolResponse::Nested(symbols)));
        }

        // not parsed yet
        Ok(None)
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        Ok(())
    }
}

fn traverse_node(node: tree_sitter::Node, rope: &ropey::Rope) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        let kind = child.kind();

        if kind == "field_declaration" {
            let mut sub_cursor = child.walk();
            for sub_child in child.children(&mut sub_cursor) {
                if sub_child.kind() == "variable_declarator" {
                    let name_node = sub_child.child_by_field_name("name").unwrap_or(sub_child);
                    let name = get_node_text(name_node, rope);

                    let range = node_range(sub_child, rope);
                    let selection_range = node_range(name_node, rope);

                    #[allow(deprecated)]
                    symbols.push(DocumentSymbol {
                        name,
                        detail: None, // TODO: field type
                        kind: SymbolKind::FIELD,
                        tags: None,
                        deprecated: None,
                        range,
                        selection_range,
                        children: None,
                    });
                }
            }
        } else {
            #[allow(deprecated)]
            let symbol_kind = match kind {
                "class_declaration" => Some(SymbolKind::CLASS),
                "interface_declaration" => Some(SymbolKind::INTERFACE),
                "method_declaration" => Some(SymbolKind::METHOD),
                "constructor_declaration" => Some(SymbolKind::CONSTRUCTOR),
                "enum_declaration" => Some(SymbolKind::ENUM),
                _ => None,
            };

            if let Some(s_kind) = symbol_kind {
                let name_node = child.child_by_field_name("name").unwrap_or(child);
                let name = get_node_text(name_node, rope);

                let children = if s_kind == SymbolKind::CLASS
                    || s_kind == SymbolKind::INTERFACE
                    || s_kind == SymbolKind::ENUM
                {
                    let inner = traverse_node(child, rope);
                    if inner.is_empty() { None } else { Some(inner) }
                } else {
                    None
                };

                let range = node_range(child, rope);
                let selection_range = node_range(name_node, rope);

                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name,
                    detail: None,
                    kind: s_kind,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range,
                    children,
                });
            } else if kind == "class_body" || kind == "program" || kind == "enum_body" {
                let mut inner = traverse_node(child, rope);
                symbols.append(&mut inner);
            }
        }
    }
    symbols
}

fn get_node_text(node: tree_sitter::Node, rope: &ropey::Rope) -> String {
    let start_char = rope.byte_to_char(node.start_byte());
    let end_char = rope.byte_to_char(node.end_byte());

    rope.slice(start_char..end_char).to_string()
}

fn node_range(node: tree_sitter::Node, _rope: &ropey::Rope) -> lsp_types::Range {
    let start_pos = node.start_position();
    let end_pos = node.end_position();

    tower_lsp::lsp_types::Range {
        start: Position::new(start_pos.row as u32, start_pos.column as u32),
        end: Position::new(end_pos.row as u32, end_pos.column as u32),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    // let stdin = tokio::io::stdin();
    // let stdout = tokio::io::stdout();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:9257").await?;
    tracing::info!("Starting lsp server at port 9257");

    let (service, socket) = LspService::new(LspBackend::new);
    let (stream, _) = listener.accept().await?;
    let (read, write) = tokio::io::split(stream);

    // Server::new(stdin, stdout, socket).serve(service).await;
    Server::new(read, write, socket).serve(service).await;

    Ok(())
}
