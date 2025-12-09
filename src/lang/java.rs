use super::LanguageService;
use crate::utils::{get_node_text, node_range};
use ropey::Rope;
use tower_lsp::lsp_types::{DocumentSymbol, SymbolKind};
use tree_sitter::{Node, Tree};

pub struct JavaService;

impl LanguageService for JavaService {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn document_symbol(&self, tree: &Tree, rope: &Rope) -> Vec<DocumentSymbol> {
        traverse_node(tree.root_node(), rope)
    }
}

fn traverse_node(node: Node, rope: &Rope) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        let kind = child.kind();

        if kind == "field_declaration" {
            let type_node = child.child_by_field_name("type");
            let type_name = type_node
                .map(|n| get_node_text(n, rope))
                .unwrap_or_default();

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
                        detail: Some(type_name.clone()),
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

                let detail = if s_kind == SymbolKind::METHOD {
                    child
                        .child_by_field_name("type")
                        .map(|n| get_node_text(n, rope))
                } else {
                    None
                };

                let children = if matches!(
                    s_kind,
                    SymbolKind::CLASS | SymbolKind::INTERFACE | SymbolKind::ENUM
                ) {
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
                    detail,
                    kind: s_kind,
                    tags: None,
                    deprecated: None,
                    range,
                    selection_range,
                    children,
                });
            } else if matches!(kind, "class_body" | "program" | "enum_body") {
                let mut inner = traverse_node(child, rope);
                symbols.append(&mut inner);
            }
        }
    }
    symbols
}
