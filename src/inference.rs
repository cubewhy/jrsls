use crate::ast::{InferredType, parse_java_type};
use crate::lang::java::JavaService;
use crate::state::GlobalIndex;
use crate::utils::get_node_text;
use ropey::Rope;
use tree_sitter::Node;

pub struct TypeSolver<'a> {
    pub rope: &'a Rope,
    pub index: &'a GlobalIndex,
    pub current_uri: &'a str,
}

impl<'a> TypeSolver<'a> {
    pub fn new(rope: &'a Rope, index: &'a GlobalIndex, current_uri: &'a str) -> Self {
        Self {
            rope,
            index,
            current_uri,
        }
    }

    pub fn infer(&self, node: Node) -> InferredType {
        match node.kind() {
            "decimal_integer_literal" => InferredType::Int,
            "decimal_floating_point_literal" => {
                let text = get_node_text(node, self.rope);
                if text.ends_with('f') || text.ends_with('F') {
                    InferredType::Float
                } else {
                    InferredType::Double
                }
            }
            "string_literal" => InferredType::String,
            "true" | "false" => InferredType::Boolean,

            "identifier" => self.resolve_variable_type(node),

            "method_invocation" => self.resolve_method_return_type(node),

            "object_creation_expression" => {
                if let Some(type_node) = node.child_by_field_name("type") {
                    return parse_java_type(type_node, self.rope);
                }
                InferredType::Unknown
            }

            "parenthesized_expression" => {
                if let Some(inner) = node.named_child(0) {
                    return self.infer(inner);
                }
                InferredType::Unknown
            }

            "cast_expression" => {
                if let Some(type_node) = node.child_by_field_name("type") {
                    return parse_java_type(type_node, self.rope);
                }
                InferredType::Unknown
            }

            _ => InferredType::Unknown,
        }
    }

    fn resolve_variable_type(&self, identifier_node: Node) -> InferredType {
        let var_name = get_node_text(identifier_node, self.rope);

        if let Some(def_node) = find_declaration_node(identifier_node, &var_name, self.rope) {
            if let Some(parent) = def_node.parent()
                && (parent.kind() == "local_variable_declaration"
                    || parent.kind() == "field_declaration")
                && let Some(type_node) = parent.child_by_field_name("type")
            {
                return parse_java_type(type_node, self.rope);
            }

            if def_node.kind() == "formal_parameter"
                && let Some(type_node) = def_node.child_by_field_name("type")
            {
                return parse_java_type(type_node, self.rope);
            }
        }

        InferredType::Unknown
    }

    // 🕵️‍♂️ 侦探 2号：查方法返回值
    fn resolve_method_return_type(&self, invocation_node: Node) -> InferredType {
        // method_invocation -> name
        if let Some(name_node) = invocation_node.child_by_field_name("name") {
            let method_name = get_node_text(name_node, self.rope);

            // 这里要小心！无限递归风险！
            // 为了查找方法的定义，我们需要解决它的参数类型来做重载匹配。
            // 但如果参数里又有方法调用，就会递归。
            // 简单起见，我们在查找定义时，先暂时只匹配名字和参数个数，不做深度类型推断。

            if let Some(def_node) =
                find_method_definition_node(invocation_node, &method_name, self.rope)
            {
                // 找到了方法定义！
                // void func() {} -> method_declaration type: (void_type)
                if let Some(type_node) = def_node.child_by_field_name("type") {
                    // 特殊处理 void
                    if type_node.kind() == "void_type" {
                        return InferredType::Unknown; // 或者加一个 Void 类型
                    }
                    return parse_java_type(type_node, self.rope);
                }
            }
        }
        InferredType::Unknown
    }
}

pub fn find_declaration_node<'tree>(
    start_node: Node<'tree>,
    target_name: &str,
    rope: &Rope,
) -> Option<Node<'tree>> {
    let mut curr = start_node;

    while let Some(parent) = curr.parent() {
        let kind = parent.kind();

        // ---------------------------------------------------------
        // 1. 检查方法/构造函数参数 (Parameters)
        // ---------------------------------------------------------
        if kind == "method_declaration" || kind == "constructor_declaration" {
            if let Some(params) = parent.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for param in params.children(&mut cursor) {
                    // 支持普通参数 (int a) 和变长参数 (int... a)
                    if param.kind() == "formal_parameter" || param.kind() == "spread_parameter" {
                        if let Some(name) = param.child_by_field_name("name") {
                            if get_node_text(name, rope) == target_name {
                                return Some(param); // 返回参数定义节点
                            }
                        }
                    }
                }
            }
        }

        // ---------------------------------------------------------
        // 2. 检查局部变量 (Local Variables) - 在 Block 作用域内
        // ---------------------------------------------------------
        if kind == "block" {
            // 遍历 block 里的所有语句
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                // 局部变量声明: int a = 1, b = 2;
                if child.kind() == "local_variable_declaration" {
                    if let Some(node) = find_in_declarators(child, target_name, rope) {
                        return Some(node);
                    }
                }
            }
        }

        // ---------------------------------------------------------
        // 3. 检查增强 For 循环 (Enhanced For Loop)
        // e.g. for (String s : list)
        // ---------------------------------------------------------
        if kind == "enhanced_for_statement" {
            // Java tree-sitter 结构：
            // (enhanced_for_statement type: (_) name: (identifier) value: (_))
            // 或者 (enhanced_for_statement (formal_parameter ...))

            // 方式 A: 直接包含 type 和 name
            if let Some(name_node) = parent.child_by_field_name("name") {
                if get_node_text(name_node, rope) == target_name {
                    // 这里 parent 本身就是定义语句，我们可以返回 parent 或者 name_node
                    // 为了让 TypeSolver 方便找 type，我们返回 parent
                    return Some(parent);
                }
            }

            // 方式 B: 使用 formal_parameter 作为子节点
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                if child.kind() == "formal_parameter" {
                    if let Some(name) = child.child_by_field_name("name") {
                        if get_node_text(name, rope) == target_name {
                            return Some(child);
                        }
                    }
                }
            }
        }

        // ---------------------------------------------------------
        // 4. 检查类成员字段 (Class Fields)
        // ---------------------------------------------------------
        if kind == "class_declaration" {
            if let Some(body) = parent.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    // 字段声明: private int a = 1;
                    if child.kind() == "field_declaration" {
                        if let Some(node) = find_in_declarators(child, target_name, rope) {
                            return Some(node);
                        }
                    }
                }
            }
        }

        // ---------------------------------------------------------
        // 5. Try-with-resources
        // try (InputStream is = ...)
        // ---------------------------------------------------------
        if kind == "resource_specification" {
            let mut cursor = parent.walk();
            for resource in parent.children(&mut cursor) {
                if resource.kind() == "resource" {
                    if let Some(name) = resource.child_by_field_name("name") {
                        if get_node_text(name, rope) == target_name {
                            return Some(resource);
                        }
                    }
                }
            }
        }

        // 继续往外层找
        curr = parent;
    }

    None
}

fn find_in_declarators<'tree>(
    declaration_node: Node<'tree>,
    target_name: &str,
    rope: &Rope,
) -> Option<Node<'tree>> {
    let mut cursor = declaration_node.walk();
    for child in declaration_node.children(&mut cursor) {
        if child.kind() == "variable_declarator" {
            if let Some(name_node) = child.child_by_field_name("name") {
                if get_node_text(name_node, rope) == target_name {
                    return Some(child);
                }
            }
        }
    }
    None
}

fn find_method_definition_node<'tree>(
    start_node: Node<'tree>,
    target_name: &str,
    rope: &Rope,
) -> Option<Node<'tree>> {
    let mut curr = start_node;
    while let Some(parent) = curr.parent() {
        if parent.kind() == "class_declaration" {
            if let Some(body) = parent.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    if child.kind() == "method_declaration" {
                        if let Some(name) = child.child_by_field_name("name") {
                            if get_node_text(name, rope) == target_name {
                                return Some(child);
                            }
                        }
                    }
                }
            }
        }
        curr = parent;
    }
    None
}
