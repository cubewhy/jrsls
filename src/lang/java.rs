use super::LanguageService;
use crate::{
    ast::get_call_args,
    state::{self, GlobalIndex},
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

        let call_args = get_call_args(node);

        tracing::info!(
            "Jump target: {}, Arg count: {:?}",
            target_name,
            call_args.len()
        );

        if node.kind() != "identifier" && node.kind() != "type_identifier" {
            return None;
        }
        let global_candidates = index.classes_by_short_name(&target_name);

        if let Some(range) =
            find_definition_in_file(node, &target_name, rope, &call_args, index, current_uri)
        {
            return Some(Location::new(
                lsp_types::Url::parse(current_uri).unwrap(),
                range,
            ));
        }

        let Some(file_info) = index.file_info(current_uri) else {
            return select_fallback(global_candidates);
        };

        if let Some(loc) =
            match_imported_symbol(&global_candidates, &file_info.imports, &target_name)
        {
            return Some(Location::new(loc.uri, loc.range));
        }

        if let Some(pkg) = &file_info.package_name
            && let Some(loc) = match_same_package(&global_candidates, pkg, &target_name)
        {
            return Some(Location::new(loc.uri, loc.range));
        }

        select_fallback(global_candidates)
    }
}

fn match_imported_symbol(
    candidates: &[state::ClassLocation],
    imports: &[String],
    target_name: &str,
) -> Option<state::ClassLocation> {
    for import in imports {
        if import.ends_with(&format!(".{}", target_name)) {
            if let Some(loc) = candidates.iter().find(|loc| &loc.fqcn == import) {
                return Some(loc.clone());
            }
        }
    }
    None
}

fn match_same_package(
    candidates: &[state::ClassLocation],
    package_name: &str,
    target_name: &str,
) -> Option<state::ClassLocation> {
    let potential_fqcn = format!("{}.{}", package_name, target_name);
    candidates
        .iter()
        .find(|loc| loc.fqcn == potential_fqcn)
        .cloned()
}

fn select_fallback(candidates: Vec<state::ClassLocation>) -> Option<Location> {
    if candidates.is_empty() {
        return None;
    }
    if let Some(java_lang) = candidates
        .iter()
        .find(|loc| loc.fqcn.starts_with("java.lang."))
    {
        return Some(Location::new(java_lang.uri.clone(), java_lang.range));
    }
    let loc = &candidates[0];
    Some(Location::new(loc.uri.clone(), loc.range))
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
