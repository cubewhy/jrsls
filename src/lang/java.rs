use super::LanguageService;
use crate::{
    ast::get_call_args,
    inference::TypeSolver,
    state::{self, GlobalIndex},
    utils::{calculate_score, find_definition_in_file, get_node_at_pos, get_node_text, node_range},
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

        if node.kind() != "identifier"
            && node.kind() != "type_identifier"
            && node.kind() != "field_identifier"
        {
            return None;
        }
        let global_candidates = index.classes_by_short_name(&target_name);
        let global_members = index.members_by_name(&target_name);
        let qualifier = resolve_qualifier(node, rope);

        if qualifier.is_none() {
            if let Some(range) =
                find_definition_in_file(node, &target_name, rope, &call_args, index, current_uri)
            {
                return Some(Location::new(
                    lsp_types::Url::parse(current_uri).unwrap(),
                    range,
                ));
            }
        }

        let Some(file_info) = index.file_info(current_uri) else {
            return select_fallback(global_candidates);
        };

        if let Some(loc) = match_same_file(&global_candidates, current_uri) {
            return Some(Location::new(loc.uri, loc.range));
        }

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

        if let Some(loc) = match_member(
            node,
            rope,
            &global_members,
            &file_info,
            index,
            qualifier.as_deref(),
            &call_args,
            current_uri,
        ) {
            return Some(loc);
        }

        if let Some(loc) = match_java_lang(&global_candidates) {
            return Some(Location::new(loc.uri, loc.range));
        }

        // Respect Java import rules: if nothing matched, do not jump.
        None
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

fn match_same_file(
    candidates: &[state::ClassLocation],
    current_uri: &str,
) -> Option<state::ClassLocation> {
    candidates
        .iter()
        .find(|loc| loc.uri.as_str() == current_uri)
        .cloned()
}

fn match_java_lang(candidates: &[state::ClassLocation]) -> Option<state::ClassLocation> {
    candidates
        .iter()
        .find(|loc| loc.fqcn.starts_with("java.lang."))
        .cloned()
}

fn select_fallback(candidates: Vec<state::ClassLocation>) -> Option<Location> {
    if candidates.is_empty() {
        return None;
    }
    let mut ordered = candidates;
    ordered.sort_by_key(|loc| {
        if loc.uri.scheme() == "file" || loc.uri.scheme() == "untitled" {
            0
        } else if loc.fqcn.starts_with("java.lang.") {
            1
        } else {
            2
        }
    });

    let loc = &ordered[0];
    Some(Location::new(loc.uri.clone(), loc.range))
}

fn match_member(
    node: Node,
    rope: &Rope,
    members: &[state::MemberLocation],
    file_info: &state::FileInfo,
    index: &GlobalIndex,
    qualifier: Option<&str>,
    call_args: &[Node],
    current_uri: &str,
) -> Option<Location> {
    // Attempt to use the qualifier's type to narrow down the member
    let qualifier = qualifier
        .map(|q| q.to_string())
        .or_else(|| resolve_qualifier(node, rope))
        .unwrap_or_default();
    let qualifier_candidates = index.classes_by_short_name(&qualifier);
    let qualifier_fqcn = resolve_qualifier_fqcn(&qualifier, &qualifier_candidates, file_info);
    let fqcn = qualifier_fqcn.clone().unwrap_or_default();
    let arg_count = count_args(node);

    let candidates: Vec<_> = members
        .iter()
        .filter(|m| fqcn.is_empty() || m.fqmn.starts_with(&format!("{}.", fqcn)))
        .filter(|m| match_member_arity(m, arg_count))
        .collect();

    tracing::debug!(
        "member resolution for {}.{}: arg_count={}, fqcn_resolved={:?}, candidates={}",
        qualifier,
        get_node_text(node, rope),
        arg_count,
        qualifier_fqcn,
        candidates.len()
    );

    if candidates.is_empty() {
        // Fallback: use any member with the same name (best-effort)
        let mut fallback: Vec<_> = members.iter().collect();
        fallback.sort_by_key(|m| {
            (
                m.is_varargs,
                priority_for_uri(&m.uri, &m.fqmn),
                m.param_count,
            )
        });
        tracing::debug!(
            "member resolution fallback for {}: arg_count={}, candidates={}",
            qualifier,
            arg_count,
            fallback.len()
        );
        return fallback
            .first()
            .map(|m| Location::new(m.uri.clone(), m.range));
    }

    let mut scored: Vec<_> = candidates
        .into_iter()
        .filter_map(|m| {
            score_member(m, call_args, rope, index, current_uri).map(|score| (m, score))
        })
        .collect();

    if scored.is_empty() {
        tracing::debug!(
            "member resolution scoring produced no candidates for {}.{}",
            qualifier,
            get_node_text(node, rope)
        );
        return None;
    }

    scored.sort_by_key(|(m, score)| {
        (
            m.is_varargs,
            -score,
            (m.param_count as isize - arg_count as isize).abs(),
            priority_for_uri(&m.uri, &m.fqmn),
        )
    });

    let member = scored[0].0;
    Some(Location::new(member.uri.clone(), member.range))
}

fn score_member(
    member: &state::MemberLocation,
    call_args: &[Node],
    rope: &Rope,
    index: &GlobalIndex,
    current_uri: &str,
) -> Option<i32> {
    let mut total = 0;
    if member.param_types.is_empty() {
        return Some(0);
    }

    let solver = TypeSolver::new(rope, index, current_uri);

    for (i, arg) in call_args.iter().enumerate() {
        let param_idx = if member.is_varargs && i >= member.param_types.len() {
            member.param_types.len().saturating_sub(1)
        } else {
            i
        };
        if param_idx >= member.param_types.len() {
            return None;
        }
        let arg_type = solver.infer(*arg);
        let param_type = &member.param_types[param_idx];
        let score = calculate_score(&arg_type, param_type);
        if score < 0 {
            tracing::debug!(
                "reject member {} due to type mismatch: arg={:?} param={:?} score={}",
                member.fqmn,
                arg_type,
                param_type,
                score
            );
            return None;
        }
        total += score;
    }

    tracing::debug!(
        "score member {} with args {} => {}",
        member.fqmn,
        call_args.len(),
        total
    );
    Some(total)
}

fn priority_for_uri(uri: &lsp_types::Url, fqmn: &str) -> i32 {
    if uri.scheme() == "file" || uri.scheme() == "untitled" {
        0
    } else if fqmn.starts_with("java.") {
        1
    } else {
        2
    }
}

fn count_args(node: Node) -> usize {
    if let Some(parent) = node.parent() {
        if parent.kind() == "method_invocation" {
            let mut cursor = parent.walk();
            let args: Vec<_> = parent
                .children_by_field_name("arguments", &mut cursor)
                .flat_map(|arglist| {
                    let mut inner = arglist.walk();
                    arglist
                        .children(&mut inner)
                        .filter(|n| n.kind() != "," && n.is_named())
                        .collect::<Vec<_>>()
                })
                .collect();
            return args.len();
        }
    }
    0
}

fn match_member_arity(member: &state::MemberLocation, arg_count: usize) -> bool {
    if member.is_varargs {
        arg_count >= member.param_count.saturating_sub(1)
    } else {
        arg_count == member.param_count
    }
}
fn resolve_qualifier(node: Node, rope: &Rope) -> Option<String> {
    // Handles both field_access (System.out) and method_invocation (obj.method())
    if let Some(parent) = node.parent() {
        if parent.kind() == "field_access" {
            if let Some(object) = parent.child_by_field_name("object") {
                return Some(get_node_text(object, rope));
            }
        }
        if parent.kind() == "method_invocation" {
            if let Some(object) = parent.child_by_field_name("object") {
                return Some(get_node_text(object, rope));
            }
        }
    }
    None
}

fn resolve_qualifier_fqcn(
    qualifier: &str,
    class_candidates: &[state::ClassLocation],
    file_info: &state::FileInfo,
) -> Option<String> {
    // If qualifier looks qualified (chained access), try the first segment as the type name.
    if qualifier.contains('.') {
        if let Some(first) = qualifier.split('.').next() {
            return resolve_qualifier_fqcn(first, class_candidates, file_info);
        }
    }

    // Try imports first
    for import in &file_info.imports {
        if import.ends_with(&format!(".{}", qualifier)) {
            return Some(import.clone());
        }
    }

    // Try same package
    if let Some(pkg) = &file_info.package_name {
        let fqcn = format!("{}.{}", pkg, qualifier);
        if class_candidates.iter().any(|c| c.fqcn == fqcn) {
            return Some(fqcn);
        }
    }

    // Implicit java.lang
    if let Some(c) = class_candidates
        .iter()
        .find(|c| c.fqcn.starts_with("java.lang."))
    {
        return Some(c.fqcn.clone());
    }

    // Fall back to any class with this short name
    class_candidates
        .iter()
        .find(|c| c.fqcn.ends_with(&format!(".{}", qualifier)) || c.fqcn == qualifier)
        .map(|c| c.fqcn.clone())
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
