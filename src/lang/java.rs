use super::LanguageService;
use crate::{
    ast::get_method_call_arg_count,
    state::GlobalIndex,
    utils::{find_definition_in_file, get_node_at_pos, get_node_text, node_range},
};
use ropey::Rope;
use tower_lsp::lsp_types::{self, DocumentSymbol, Location, Position, SymbolKind};
use tree_sitter::{Node, Tree};

pub struct JavaService;

impl LanguageService for JavaService {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn document_symbol(&self, tree: &Tree, rope: &Rope) -> Vec<DocumentSymbol> {
        traverse_node(tree.root_node(), rope)
    }

    fn goto_definition(
        &self,
        tree: &Tree,
        rope: &Rope,
        position: Position,
        index: &GlobalIndex,
        current_uri: &str,
    ) -> Option<Location> {
        let (node, target_name) = get_node_at_pos(tree, rope, position)?;

        let expected_argc = get_method_call_arg_count(node);

        tracing::info!(
            "Jump target: {}, Arg count: {:?}",
            target_name,
            expected_argc
        );

        if node.kind() != "identifier" && node.kind() != "type_identifier" {
            return None;
        }

        if let Some(range) = find_definition_in_file(node, &target_name, rope, expected_argc) {
            return Some(Location::new(
                lsp_types::Url::parse(current_uri).unwrap(),
                range,
            ));
        }

        // TODO: overload support for global index

        if let Some(file_info) = index.file_info.get(current_uri) {
            // match import
            for import in &file_info.imports {
                if import.ends_with(&format!(".{}", target_name))
                    && let Some(candidates) = index.short_name_map.get(&target_name)
                {
                    for loc in candidates.value() {
                        if &loc.fqcn == import {
                            return Some(Location::new(loc.uri.clone(), loc.range));
                        }
                    }
                }
            }

            // match same package
            if let Some(pkg) = &file_info.package_name {
                let potential_fqcn = format!("{}.{}", pkg, target_name);
                if let Some(candidates) = index.short_name_map.get(&target_name) {
                    for loc in candidates.value() {
                        if loc.fqcn == potential_fqcn {
                            return Some(Location::new(loc.uri.clone(), loc.range));
                        }
                    }
                }
            }
        }

        // match short name
        if let Some(candidates) = index.short_name_map.get(&target_name)
            && let Some(loc) = candidates.first()
        {
            // 这里其实可以做的更好：如果是 java.lang 的类优先返回，否则返回第一个
            // 目前逻辑：直接跳到第一个同名类的定义处
            return Some(Location::new(loc.uri.clone(), loc.range));
        }

        None
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
