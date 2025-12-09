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

pub fn find_definition_in_file(start_node: Node, target_name: &str, rope: &Rope) -> Option<Range> {
    let mut curr = start_node;

    while let Some(parent) = curr.parent() {
        let kind = parent.kind();

        if kind == "method_declaration" {
            if let Some(params) = parent.child_by_field_name("parameters") {
                if let Some(range) = search_scope(params, target_name, rope) {
                    return Some(range);
                }
            }
            if let Some(body) = parent.child_by_field_name("body") {
                if let Some(range) = search_scope(body, target_name, rope) {
                    return Some(range);
                }
            }
        }

        if kind == "class_declaration" {
            if let Some(body) = parent.child_by_field_name("body") {
                if let Some(range) = search_fields_in_class(body, target_name, rope) {
                    return Some(range);
                }
            }
        }

        curr = parent;
    }

    None
}

pub fn search_scope(scope_node: Node, target_name: &str, rope: &Rope) -> Option<Range> {
    let mut cursor = scope_node.walk();

    for child in scope_node.children(&mut cursor) {
        if child.kind() == "local_variable_declaration" {
            let mut sub_cursor = child.walk();
            for sub in child.children(&mut sub_cursor) {
                if sub.kind() == "variable_declarator" {
                    if let Some(name_node) = sub.child_by_field_name("name") {
                        if get_node_text(name_node, rope) == target_name {
                            return Some(node_range(name_node, rope));
                        }
                    }
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
                if sub.kind() == "variable_declarator" {
                    if let Some(name_node) = sub.child_by_field_name("name") {
                        if get_node_text(name_node, rope) == target_name {
                            return Some(node_range(name_node, rope));
                        }
                    }
                }
            }
        }

        if child.kind() == "method_declaration" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if get_node_text(name_node, rope) == target_name {
                    return Some(node_range(name_node, rope));
                }
            }
        }
    }
    None
}
