use ropey::Rope;
use tree_sitter::Node;

use crate::utils::get_node_text;

#[derive(Debug, PartialEq, Clone)]
pub enum InferredType {
    Int,
    Long,
    Boolean,
    Char,
    String,
    Float,
    Double,

    Class(String),

    Unknown,
}

pub fn infer_expr_type(node: Node, rope: &Rope) -> InferredType {
    match node.kind() {
        // 1. 字面量处理
        "decimal_integer_literal" => {
            let text = get_node_text(node, rope);
            if text.ends_with('L') || text.ends_with('l') {
                InferredType::Long
            } else {
                InferredType::Int
            }
        }
        "decimal_floating_point_literal" => {
            let text = get_node_text(node, rope);
            if text.ends_with('f') || text.ends_with('F') {
                InferredType::Float
            } else {
                InferredType::Double
            }
        }
        "string_literal" => InferredType::String,
        "true" | "false" => InferredType::Boolean,
        "character_literal" => InferredType::Char,

        "cast_expression" => {
            if let Some(type_node) = node.child_by_field_name("type") {
                return parse_java_type(type_node, rope);
            }
            InferredType::Unknown
        }

        "parenthesized_expression" => {
            if let Some(inner) = node.named_child(0) {
                return infer_expr_type(inner, rope);
            }
            InferredType::Unknown
        }

        "identifier" => InferredType::Unknown,

        _ => InferredType::Unknown,
    }
}

pub fn parse_java_type(type_node: Node, rope: &Rope) -> InferredType {
    let text = get_node_text(type_node, rope);

    match text.as_str() {
        "int" | "Integer" => InferredType::Int,
        "long" | "Long" => InferredType::Long,
        "boolean" | "Boolean" => InferredType::Boolean,
        "char" | "Character" => InferredType::Char,
        "float" | "Float" => InferredType::Float,
        "double" | "Double" => InferredType::Double,
        "String" => InferredType::String,
        _ => InferredType::Class(text),
    }
}

pub fn get_call_args(node: Node) -> Vec<Node> {
    if let Some(parent) = node.parent()
        && parent.kind() == "method_invocation"
        && let Some(args_node) = parent.child_by_field_name("arguments")
    {
        let mut cursor = args_node.walk();

        return args_node
            .children(&mut cursor)
            .filter(|n| n.is_named())
            .collect();
    }

    Vec::new()
}

pub fn get_def_param_types(method_node: Node, rope: &Rope) -> Vec<String> {
    let mut types = Vec::new();
    if let Some(params) = method_node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for param in params.children(&mut cursor) {
            // formal_parameter -> type(type_identifier)
            if param.kind() == "formal_parameter"
                && let Some(type_node) = param.child_by_field_name("type")
            {
                let start = rope.byte_to_char(type_node.start_byte());
                let end = rope.byte_to_char(type_node.end_byte());
                types.push(rope.slice(start..end).to_string());
            }
        }
    }
    types
}

pub fn get_method_call_arg_count(node: Node) -> Option<usize> {
    if let Some(parent) = node.parent()
        && parent.kind() == "method_invocation"
        && let Some(args_node) = parent.child_by_field_name("arguments")
    {
        return Some(args_node.named_child_count());
    }

    None
}

pub fn get_method_def_param_count(method_node: Node) -> Option<usize> {
    if method_node.kind() == "method_declaration"
        && let Some(params_node) = method_node.child_by_field_name("parameters")
    {
        return Some(params_node.named_child_count());
    }

    None
}
