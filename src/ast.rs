use tree_sitter::Node;

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
