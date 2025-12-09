use super::LanguageService;
use crate::{
    ast::get_call_args,
    inference::TypeSolver,
    state::{self, GlobalIndex},
    utils::{calculate_score, find_definition_in_file, get_node_at_pos, get_node_text, node_range},
};
use ropey::Rope;
use std::collections::HashSet;
use tower_lsp::lsp_types::{
    self, CompletionItem, CompletionItemKind, DocumentSymbol, Location, Position, SymbolKind,
};
use tree_sitter::{Node, Tree};

pub struct JavaService;

impl LanguageService for JavaService {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn document_symbol(&self, tree: &Tree, rope: &Rope) -> Vec<DocumentSymbol> {
        traverse_node(tree.root_node(), rope)
    }

    fn completion(
        &self,
        tree: &Tree,
        rope: &Rope,
        position: Position,
        index: &GlobalIndex,
        current_uri: &str,
        keywords: &[String],
    ) -> Option<Vec<CompletionItem>> {
        let byte_idx = offset_for_position(rope, position)?;
        let prev_char = byte_before(rope, byte_idx);

        if let Some(ctx) = member_completion_context(tree, rope, position, prev_char) {
            let file_info = index.file_info(current_uri)?;
            let qualifier_fqcn =
                resolve_qualifier_for_completion(&ctx.qualifier, index, &file_info, tree, rope)?;

            let members = index.members_of_class(&qualifier_fqcn);
            let mut seen = HashSet::new();
            let items = members
                .into_iter()
                .filter(|m| seen.insert(m.fqmn.clone()))
                .filter(|m| {
                    m.fqmn
                        .split('.')
                        .last()
                        .map(|name| name.starts_with(&ctx.prefix))
                        .unwrap_or(true)
                })
                .map(|m| CompletionItem {
                    label: m
                        .fqmn
                        .split('.')
                        .last()
                        .unwrap_or_else(|| m.fqmn.as_str())
                        .to_string(),
                    kind: Some(if m.is_field {
                        CompletionItemKind::FIELD
                    } else {
                        CompletionItemKind::METHOD
                    }),
                    detail: Some(qualifier_fqcn.clone()),
                    ..CompletionItem::default()
                })
                .collect::<Vec<_>>();

            tracing::debug!(
                "completion: qualifier={} fqcn={} prefix='{}' items={}",
                ctx.qualifier,
                qualifier_fqcn,
                ctx.prefix,
                items.len()
            );
            return Some(items);
        }

        // Offer classes defined in the current file and imported types as a light baseline
        let Some(file_info) = index.file_info(current_uri) else {
            return None;
        };

        let mut items = Vec::new();
        let mut seen = HashSet::new();

        for class in &file_info.defined_classes {
            if seen.insert(class.clone()) {
                items.push(CompletionItem {
                    label: class.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    ..CompletionItem::default()
                });
            }
        }

        for import in &file_info.imports {
            if let Some(short) = import.split('.').last() {
                if seen.insert(short.to_string()) {
                    items.push(CompletionItem {
                        label: short.to_string(),
                        kind: Some(CompletionItemKind::CLASS),
                        detail: Some(import.clone()),
                        ..CompletionItem::default()
                    });
                }
            }
        }

        // include java.lang common classes (String, System, Object) if available
        for base in ["String", "System", "Object", "Exception", "Throwable"] {
            if seen.contains(base) {
                continue;
            }
            if let Some(loc) = index
                .classes_by_short_name(base)
                .into_iter()
                .find(|c| c.fqcn.starts_with("java.lang."))
            {
                items.push(CompletionItem {
                    label: base.to_string(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some(loc.fqcn),
                    ..CompletionItem::default()
                });
                seen.insert(base.to_string());
            }
        }

        if !prev_char.map(|c| c.is_alphanumeric()).unwrap_or(false) {
            for kw in keywords {
                items.push(CompletionItem {
                    label: kw.clone(),
                    kind: Some(CompletionItemKind::KEYWORD),
                    ..CompletionItem::default()
                });
            }
        }

        // Only suggest keywords in free-form contexts (not mid-identifier and not after '.')
        if prev_char.map(|c| c.is_alphanumeric()).unwrap_or(false) {
            // skip keywords
        } else {
            for kw in keywords {
                items.push(CompletionItem {
                    label: kw.clone(),
                    kind: Some(CompletionItemKind::KEYWORD),
                    ..CompletionItem::default()
                });
            }
        }

        if items.is_empty() { None } else { Some(items) }
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

        if let Some(range) = find_local_variable(node, rope, &target_name) {
            return Some(Location::new(
                lsp_types::Url::parse(current_uri).unwrap(),
                range,
            ));
        }

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

        if let Some(loc) = match_same_file(&global_candidates, current_uri) {
            return Some(Location::new(loc.uri, loc.range));
        }

        let allow_member_lookup = qualifier.is_some()
            || node.kind() == "field_identifier"
            || node
                .parent()
                .is_some_and(|p| p.kind() == "method_invocation" || p.kind() == "field_access");

        if allow_member_lookup
            && let Some(loc) = match_member(
                node,
                rope,
                &global_members,
                &file_info,
                index,
                qualifier.as_deref(),
                &call_args,
                current_uri,
                node.parent()
                    .is_some_and(|p| p.kind() == "method_invocation"),
            )
        {
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
    prefer_method_usage_hint: bool,
) -> Option<Location> {
    // Attempt to use the qualifier's type to narrow down the member
    let qualifier = qualifier
        .map(|q| q.to_string())
        .or_else(|| resolve_qualifier(node, rope))
        .unwrap_or_default();
    if qualifier.is_empty() && !prefer_method_usage_hint {
        tracing::debug!(
            "skip member resolution: no qualifier for {}",
            get_node_text(node, rope)
        );
        return None;
    }
    let qualifier_fqcn =
        resolve_qualifier_type(node, rope, &qualifier, index, file_info);
    let fqcn = qualifier_fqcn.clone().unwrap_or_default();
    let arg_count = count_args(node);
    let prefer_method_usage = has_ancestor_kind(node, "method_invocation")
        || prefer_method_usage_hint
        || is_followed_by_paren(node, rope);
    let prefer_field_usage = !prefer_method_usage && call_args.is_empty();

    let candidates: Vec<_> = members
        .iter()
        .filter(|m| fqcn.is_empty() || m.fqmn.starts_with(&format!("{}.", fqcn)))
        .filter(|m| !prefer_method_usage || !m.is_field)
        .filter(|m| match_member_arity(m, arg_count))
        .collect();

    tracing::debug!(
        "member resolution for {}.{}: arg_count={}, fqcn_resolved={:?}, candidates={}, prefer_method={}, prefer_field={}",
        qualifier,
        get_node_text(node, rope),
        arg_count,
        qualifier_fqcn,
        candidates.len(),
        prefer_method_usage,
        prefer_field_usage
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
            prefer_field_usage.then(|| !m.is_field).unwrap_or(false),
            prefer_method_usage.then(|| m.is_field).unwrap_or(false),
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

fn has_ancestor_kind(node: Node, kind: &str) -> bool {
    let mut curr = Some(node);
    while let Some(n) = curr {
        if n.kind() == kind {
            return true;
        }
        curr = n.parent();
    }
    false
}

fn is_followed_by_paren(node: Node, rope: &Rope) -> bool {
    let end_char = rope.byte_to_char(node.end_byte());
    let mut iter = rope.chars_at(end_char);
    while let Some(ch) = iter.next() {
        if ch.is_whitespace() {
            continue;
        }
        return ch == '(';
    }
    false
}

fn match_member_arity(member: &state::MemberLocation, arg_count: usize) -> bool {
    if member.is_varargs {
        arg_count >= member.param_count.saturating_sub(1)
    } else {
        arg_count == member.param_count
    }
}

fn find_local_variable(node: Node, rope: &Rope, name: &str) -> Option<lsp_types::Range> {
    let target_byte = node.start_byte();
    let mut curr = Some(node);

    while let Some(n) = curr {
        if n.kind() == "method_declaration" {
            if let Some(params) = n.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for p in params.children(&mut cursor) {
                    if (p.kind() == "formal_parameter" || p.kind() == "spread_parameter")
                        && let Some(name_node) = p.child_by_field_name("name")
                        && get_node_text(name_node, rope) == name
                    {
                        return Some(node_range(name_node, rope));
                    }
                }
            }
        }

        if matches!(n.kind(), "block" | "method_declaration" | "program") {
            let mut cursor = n.walk();
            for child in n.children(&mut cursor) {
                if child.start_byte() >= target_byte {
                    break;
                }
                if child.kind() == "local_variable_declaration" {
                    let mut sub = child.walk();
                    for var in child.children(&mut sub) {
                        if var.kind() == "variable_declarator"
                            && let Some(name_node) = var.child_by_field_name("name")
                            && get_node_text(name_node, rope) == name
                        {
                            return Some(node_range(name_node, rope));
                        }
                    }
                }
            }
        }

        curr = n.parent();
    }

    None
}

fn offset_for_position(rope: &Rope, position: Position) -> Option<usize> {
    let line = position.line as usize;
    if line >= rope.len_lines() {
        return None;
    }
    let char_idx = rope.line_to_char(line) + position.character as usize;
    Some(rope.char_to_byte(char_idx))
}

fn position_before(rope: &Rope, position: Position) -> Option<Position> {
    if position.character > 0 {
        return Some(Position::new(position.line, position.character - 1));
    }
    if position.line == 0 {
        return None;
    }
    let prev_line = position.line - 1;
    let prev_len = rope.line(prev_line as usize).len_chars() as u32;
    Some(Position::new(prev_line, prev_len.saturating_sub(1)))
}

fn byte_before(rope: &Rope, byte_idx: usize) -> Option<char> {
    if byte_idx == 0 {
        return None;
    }
    let char_idx = rope.byte_to_char(byte_idx).checked_sub(1)?;
    Some(rope.char(char_idx))
}

fn qualifier_at_dot(tree: &Tree, rope: &Rope, position: Position) -> Option<String> {
    let lookup = position_before(rope, position)?;
    let byte = offset_for_position(rope, lookup)?;
    let node = tree.root_node().descendant_for_byte_range(byte, byte)?;
    let target = if node.kind() == "identifier" || node.kind() == "type_identifier" {
        node
    } else {
        node.parent()?
    };
    Some(get_node_text(target, rope))
}

fn resolve_qualifier_for_completion(
    qualifier: &str,
    index: &GlobalIndex,
    file_info: &state::FileInfo,
    tree: &Tree,
    rope: &Rope,
) -> Option<String> {
    if let Some(fqcn) = resolve_qualifier_chain(qualifier, index, file_info) {
        return Some(fqcn);
    }

    // Try to infer variable type from local declarations
    if let Some(type_name) = find_identifier_type(tree.root_node(), rope, qualifier) {
        if let Some(fqcn) = resolve_class_from_name(&type_name, index, Some(file_info)) {
            return Some(fqcn);
        }
        return Some(type_name);
    }

    if let Some(type_name) = find_type_by_text_scan(rope, qualifier) {
        if let Some(fqcn) = resolve_class_from_name(&type_name, index, Some(file_info)) {
            return Some(fqcn);
        }
        return Some(type_name);
    }

    None
}

struct MemberContext {
    qualifier: String,
    prefix: String,
}

fn member_completion_context(
    tree: &Tree,
    rope: &Rope,
    position: Position,
    prev_char: Option<char>,
) -> Option<MemberContext> {
    // Directly after dot
    if prev_char == Some('.') {
        let qualifier = qualifier_at_dot(tree, rope, position)?;
        return Some(MemberContext {
            qualifier,
            prefix: String::new(),
        });
    }

    // If cursor is inside an identifier that is part of a field access or method invocation
    let byte_idx = offset_for_position(rope, position)?;
    let node = tree
        .root_node()
        .descendant_for_byte_range(byte_idx.saturating_sub(1), byte_idx.saturating_sub(1))?;

    if node.kind() == "identifier" || node.kind() == "field_identifier" {
        if let Some(parent) = node.parent() {
            if parent.kind() == "field_access" {
                let object = parent.child_by_field_name("object")?;
                let qualifier = get_node_text(object, rope);
                let prefix = slice_prefix(node, rope, position);
                return Some(MemberContext { qualifier, prefix });
            }
        }
    }

    if node.kind() == "identifier" {
        if let Some(parent) = node.parent() {
            if parent.kind() == "method_invocation" {
                if let Some(object) = parent.child_by_field_name("object") {
                    let qualifier = get_node_text(object, rope);
                    let prefix = slice_prefix(node, rope, position);
                    return Some(MemberContext { qualifier, prefix });
                }
            }
        }
    }

    // Fallback to textual split: find nearest '.' before cursor
    textual_member_context(rope, position)
}

fn slice_prefix(node: Node, rope: &Rope, position: Position) -> String {
    let node_start = rope.byte_to_char(node.start_byte());
    let caret_char = rope.line_to_char(position.line as usize) + position.character as usize;
    if caret_char <= node_start {
        return String::new();
    }
    let end = caret_char.min(rope.byte_to_char(node.end_byte()));
    rope.slice(node_start..end).to_string()
}

fn textual_member_context(rope: &Rope, position: Position) -> Option<MemberContext> {
    let caret_char = rope.line_to_char(position.line as usize) + position.character as usize;
    let line_start = rope.line_to_char(position.line as usize);
    let text = rope.slice(line_start..caret_char).to_string();
    if let Some(dot_idx) = text.rfind('.') {
        let qualifier = text[..dot_idx].trim().to_string();
        let prefix = text[dot_idx + 1..].to_string();
        if !qualifier.is_empty() {
            return Some(MemberContext { qualifier, prefix });
        }
    }
    None
}

fn resolve_class_from_name(
    name: &str,
    index: &GlobalIndex,
    file_info: Option<&state::FileInfo>,
) -> Option<String> {
    let candidates = index.classes_by_short_name(name);
    if candidates.is_empty() {
        return None;
    }

    if let Some(info) = file_info {
        if let Some(loc) = match_imported_symbol(&candidates, &info.imports, name) {
            return Some(loc.fqcn);
        }
        if let Some(pkg) = &info.package_name {
            if let Some(loc) = match_same_package(&candidates, pkg, name) {
                return Some(loc.fqcn);
            }
        }
    }

    if let Some(loc) = match_java_lang(&candidates) {
        return Some(loc.fqcn);
    }

    candidates.first().map(|c| c.fqcn.clone())
}

fn resolve_qualifier_chain(
    qualifier: &str,
    index: &GlobalIndex,
    file_info: &state::FileInfo,
) -> Option<String> {
    let parts = qualifier.split('.').collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }

    let mut current_fqcn = resolve_class_from_name(parts[0], index, Some(file_info))?;

    for part in parts.iter().skip(1) {
        let members = index.members_of_class(&current_fqcn);
        let field = members
            .iter()
            .find(|m| m.is_field && m.fqmn.ends_with(&format!(".{}", part)));
        let field_type = field.and_then(|f| f.field_type.clone());

        let type_name = match field_type {
            Some(crate::ast::InferredType::Class(name)) => name,
            _ => return None,
        };

        current_fqcn =
            resolve_class_from_name(&type_name, index, Some(file_info)).unwrap_or(type_name);
    }

    Some(current_fqcn)
}

fn find_identifier_type(root: Node, rope: &Rope, name: &str) -> Option<String> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "local_variable_declaration" || node.kind() == "field_declaration" {
            if let Some(t) = node.child_by_field_name("type") {
                let mut sub_cursor = node.walk();
                for child in node.children(&mut sub_cursor) {
                    if child.kind() == "variable_declarator"
                        && let Some(n) = child.child_by_field_name("name")
                        && get_node_text(n, rope) == name
                    {
                        return Some(get_node_text(t, rope));
                    }
                }
            }
        }

        let mut child_cursor = node.walk();
        for child in node.children(&mut child_cursor) {
            stack.push(child);
        }
    }
    None
}

fn find_type_by_text_scan(rope: &Rope, name: &str) -> Option<String> {
    for line in rope.lines() {
        let text = line.to_string();
        if !text.contains(name) {
            continue;
        }
        let tokens: Vec<_> = text
            .split(|c: char| c.is_whitespace() || c == ';' || c == '=')
            .filter(|s| !s.is_empty())
            .collect();
        if tokens.len() >= 2 {
            let ty = tokens[0];
            let var = tokens[1];
            if var == name {
                return Some(ty.to_string());
            }
        }
    }
    None
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

fn resolve_qualifier_type(
    node: Node,
    rope: &Rope,
    qualifier: &str,
    index: &GlobalIndex,
    file_info: &state::FileInfo,
) -> Option<String> {
    // Try direct class resolution first
    if let Some(fqcn) =
        resolve_qualifier_fqcn(qualifier, &index.classes_by_short_name(qualifier), file_info)
    {
        return Some(fqcn);
    }

    // Try field chain resolution (e.g., System.out -> PrintStream)
    if let Some(fqcn) = resolve_qualifier_chain(qualifier, index, file_info) {
        return Some(fqcn);
    }

    // Try local variable/type inference by scanning declarations
    if let Some(type_name) = find_identifier_type(root_of(node), rope, qualifier)
        .or_else(|| find_type_by_text_scan(rope, qualifier))
    {
        if let Some(fqcn) = resolve_class_from_name(&type_name, index, Some(file_info)) {
            return Some(fqcn);
        }
        return Some(type_name);
    }

    None
}

fn root_of(mut node: Node) -> Node {
    while let Some(p) = node.parent() {
        node = p;
    }
    node
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
