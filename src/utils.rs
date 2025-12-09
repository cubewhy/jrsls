use ropey::Rope;
use tower_lsp::lsp_types::{Position, Range};

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
