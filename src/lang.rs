use ropey::Rope;
use tower_lsp::lsp_types::{DocumentSymbol, Location, Position};
use tree_sitter::Tree;

use crate::state::GlobalIndex;

pub trait LanguageService: Send + Sync {
    fn language(&self) -> tree_sitter::Language;

    fn document_symbol(&self, tree: &Tree, rope: &Rope) -> Vec<DocumentSymbol>;

    fn goto_definition(
        &self,
        tree: &Tree,
        rope: &Rope,
        position: Position,
        index: &GlobalIndex,
        current_uri: &str,
    ) -> Option<Location>;
}

pub mod java;
