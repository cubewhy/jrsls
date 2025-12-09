use ropey::Rope;
use tower_lsp::lsp_types::DocumentSymbol;
use tree_sitter::Tree;

pub trait LanguageService: Send + Sync {
    fn language(&self) -> tree_sitter::Language;

    fn document_symbol(&self, tree: &Tree, rope: &Rope) -> Vec<DocumentSymbol>;
}

pub mod java;
