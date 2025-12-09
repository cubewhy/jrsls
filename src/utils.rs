use crate::{
    ast::{InferredType, parse_java_type},
    inference::TypeSolver,
    state::GlobalIndex,
};
use ropey::Rope;
use tower_lsp::lsp_types::{Position, Range};
use tree_sitter::Node;

pub fn get_node_text(node: tree_sitter::Node, rope: &Rope) -> String {
    let start_char = rope.byte_to_char(node.start_byte());
    let end_char = rope.byte_to_char(node.end_byte());
    rope.slice(start_char..end_char).to_string()
}

pub fn node_range(node: tree_sitter::Node, _rope: &Rope) -> Range {
    let start_pos = node.start_position();
    let end_pos = node.end_position();

    Range {
        start: Position::new(start_pos.row as u32, start_pos.column as u32),
        end: Position::new(end_pos.row as u32, end_pos.column as u32),
    }
}

pub fn get_node_at_pos<'a>(
    tree: &'a tree_sitter::Tree,
    rope: &Rope,
    position: Position,
) -> Option<(tree_sitter::Node<'a>, String)> {
    let line = position.line as usize;
    let char_col = position.character as usize;
    let char_idx = rope.line_to_char(line) + char_col;
    let byte_idx = rope.char_to_byte(char_idx);

    let root = tree.root_node();
    let node = root.descendant_for_byte_range(byte_idx, byte_idx)?;

    if node.kind() != "identifier" && node.kind() != "type_identifier" {
        return None;
    }
    let name = get_node_text(node, rope);
    Some((node, name))
}

pub fn find_definition_in_file(
    start_node: Node,
    target_name: &str,
    rope: &Rope,
    call_args: &[Node],
    index: &GlobalIndex,
    uri: &str,
) -> Option<Range> {
    let mut curr = start_node;

    while let Some(parent) = curr.parent() {
        let kind = parent.kind();

        if kind == "method_declaration" {
            if let Some(params) = parent.child_by_field_name("parameters")
                && let Some(range) = search_scope(params, target_name, rope)
            {
                return Some(range);
            }

            if let Some(body) = parent.child_by_field_name("body")
                && let Some(range) = search_scope(body, target_name, rope)
            {
                return Some(range);
            }
        }

        if kind == "class_declaration"
            && let Some(body) = parent.child_by_field_name("body")
            && let Some(range) = search_class_member(
                body,
                target_name,
                rope,
                call_args,
                index,
                uri,
                prefer_field_first(start_node),
            )
        {
            return Some(range);
        }

        curr = parent;
    }

    None
}

fn prefer_field_first(node: Node) -> bool {
    // When there are no call arguments and we are not in a method_invocation,
    // prefer fields over methods with the same name.
    if node
        .parent()
        .is_some_and(|p| p.kind() == "method_invocation")
    {
        return false;
    }
    true
}

pub fn search_scope(scope_node: Node, target_name: &str, rope: &Rope) -> Option<Range> {
    let mut cursor = scope_node.walk();

    for child in scope_node.children(&mut cursor) {
        if child.kind() == "local_variable_declaration" {
            let mut sub_cursor = child.walk();
            for sub in child.children(&mut sub_cursor) {
                if sub.kind() == "variable_declarator"
                    && let Some(name_node) = sub.child_by_field_name("name")
                    && get_node_text(name_node, rope) == target_name
                {
                    return Some(node_range(name_node, rope));
                }
            }
        }
    }
    None
}

pub fn search_fields_in_class(class_body: Node, target_name: &str, rope: &Rope) -> Option<Range> {
    let mut cursor = class_body.walk();

    for child in class_body.children(&mut cursor) {
        if child.kind() == "field_declaration" {
            let mut sub_cursor = child.walk();

            for sub in child.children(&mut sub_cursor) {
                if sub.kind() == "variable_declarator"
                    && let Some(name_node) = sub.child_by_field_name("name")
                    && get_node_text(name_node, rope) == target_name
                {
                    return Some(node_range(name_node, rope));
                }
            }
        }

        if child.kind() == "method_declaration"
            && let Some(name_node) = child.child_by_field_name("name")
            && get_node_text(name_node, rope) == target_name
        {
            return Some(node_range(name_node, rope));
        }
    }
    None
}

pub fn calculate_score(arg_type: &InferredType, param_type: &InferredType) -> i32 {
    if arg_type == param_type {
        return 100;
    }

    match (arg_type, param_type) {
        (InferredType::Unknown, _) => 1,

        (InferredType::Int, InferredType::Long) => 50,
        (InferredType::Int, InferredType::Float) => 40,
        (InferredType::Int, InferredType::Double) => 50,

        (InferredType::Long, InferredType::Float) => 40,
        (InferredType::Long, InferredType::Double) => 50,

        (InferredType::Float, InferredType::Double) => 50,

        (InferredType::Double, InferredType::Float) => -100,
        (InferredType::Double, InferredType::Int) => -100,

        (InferredType::Class(a), InferredType::Class(b)) => {
            if a == b { 100 } else { 0 } // TODO: handle class inherits
        }

        _ => -100,
    }
}

fn search_class_member(
    class_body: Node,
    target_name: &str,
    rope: &Rope,
    call_args: &[Node],
    index: &GlobalIndex,
    uri: &str,
    prefer_field: bool,
) -> Option<Range> {
    let mut cursor = class_body.walk();

    let mut best_candidate: Option<Range> = None;
    let mut max_score = -9999;

    for child in class_body.children(&mut cursor) {
        if prefer_field {
            if let Some(range) = find_field_in_declaration(child, target_name, rope) {
                return Some(range);
            }
        }

        if child.kind() == "method_declaration" {
            let name_node = child.child_by_field_name("name")?;
            if get_node_text(name_node, rope) != target_name {
                continue;
            }

            let params_node = child.child_by_field_name("parameters")?;
            let mut p_cursor = params_node.walk();
            let def_params: Vec<Node> = params_node
                .children(&mut p_cursor)
                .filter(|n| n.kind() == "formal_parameter" || n.kind() == "spread_parameter")
                .collect();

            let mut current_score = 0;
            let mut mismatch = false;

            let has_varargs = def_params
                .last()
                .is_some_and(|p| p.kind() == "spread_parameter");
            let required_params = if has_varargs {
                def_params.len().saturating_sub(1)
            } else {
                def_params.len()
            };

            if (!has_varargs && call_args.len() != required_params)
                || (has_varargs && call_args.len() < required_params)
            {
                continue;
            }

            for (i, arg_node) in call_args.iter().enumerate() {
                let def_param_idx = if has_varargs && i >= required_params {
                    def_params.len() - 1
                } else {
                    i
                };

                let def_param = def_params[def_param_idx];
                let Some(def_type_node) = def_param.child_by_field_name("type") else {
                    mismatch = true;
                    break;
                };

                let solver = TypeSolver::new(rope, index, uri);
                let arg_type = solver.infer(*arg_node);

                let param_type = parse_java_type(def_type_node, rope);

                let score = calculate_score(&arg_type, &param_type);

                if score < 0 {
                    mismatch = true;
                    break;
                }
                current_score += score;
            }

            if mismatch {
                continue;
            }

            if current_score > max_score {
                max_score = current_score;
                best_candidate = Some(node_range(name_node, rope));

                tracing::info!(
                    "Found candidate for {}: score={}, types matched perfectly",
                    target_name,
                    max_score
                );
            }
        }

        if !prefer_field {
            if let Some(range) = find_field_in_declaration(child, target_name, rope) {
                return Some(range);
            }
        }
    }

    best_candidate
}

fn find_field_in_declaration(node: Node, target_name: &str, rope: &Rope) -> Option<Range> {
    if node.kind() != "field_declaration" {
        return None;
    }
    let mut sub_cursor = node.walk();

    for sub in node.children(&mut sub_cursor) {
        if sub.kind() == "variable_declarator"
            && let Some(name_node) = sub.child_by_field_name("name")
            && get_node_text(name_node, rope) == target_name
        {
            return Some(node_range(name_node, rope));
        }
    }
    None
}
