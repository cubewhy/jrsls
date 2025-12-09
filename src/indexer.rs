use crate::state::{IndexedClass, IndexedMember};
use crate::utils::{get_node_text, node_range};
use ropey::Rope;
use tower_lsp::lsp_types;
use tree_sitter::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

use crate::state::GlobalIndex;

lazy_static::lazy_static! {
    static ref JAVA_QUERY: Query = Query::new(
        &tree_sitter_java::LANGUAGE.into(),
        r#"
        (package_declaration (scoped_identifier) @package)
        (import_declaration (scoped_identifier) @import)
        (class_declaration name: (identifier) @class)
        (interface_declaration name: (identifier) @interface)
        (enum_declaration name: (identifier) @enum)
        (record_declaration name: (identifier) @record)
        (annotation_type_declaration name: (identifier) @annotation)
        "#
    ).unwrap();
}

pub struct Indexer;

impl Indexer {
    pub fn update_file(index: &GlobalIndex, uri: &str, tree: &tree_sitter::Tree, rope: &Rope) {
        let mut cursor = QueryCursor::new();
        let source = rope.to_string();

        let mut capture_iter = cursor.captures(&JAVA_QUERY, tree.root_node(), source.as_bytes());

        let mut package_name = None;
        let mut imports = Vec::new();
        let mut defined_classes = Vec::new();
        let mut indexed_classes = Vec::new();
        let mut indexed_members = Vec::new();

        let url = match lsp_types::Url::parse(uri) {
            Ok(u) => u,
            Err(_) => return,
        };

        while let Some((m, capture_idx_ref)) = capture_iter.next() {
            let capture_index = *capture_idx_ref;

            let capture = m.captures[capture_index];
            let node = capture.node;

            let text = get_node_text(node, rope);

            let query_capture_idx = capture.index as usize;
            let capture_name = JAVA_QUERY.capture_names()[query_capture_idx];

            match capture_name {
                "package" => package_name = Some(text),
                "import" => imports.push(text),
                "class" | "interface" | "enum" | "record" | "annotation" => {
                    defined_classes.push(text.clone());

                    let fqcn = package_name
                        .as_ref()
                        .map(|pkg| format!("{}.{}", pkg, text))
                        .unwrap_or(text.clone());

                    let class_range = node_range(node.parent().unwrap_or(node), rope);

                    indexed_classes.push(IndexedClass {
                        short_name: text.clone(),
                        fqcn: fqcn.clone(),
                        uri: url.clone(),
                        range: class_range,
                    });

                    // collect members from class body
                    if let Some(class_node) = node.parent()
                        && let Some(body) = class_node.child_by_field_name("body")
                    {
                        collect_members(body, &fqcn, &mut indexed_members, &url, rope);
                    }
                }
                _ => {}
            }
        }

        index.upsert_file(uri, package_name, imports, indexed_classes, indexed_members);

        tracing::debug!("Indexed {}: classes={:?}", uri, defined_classes);
    }
}

fn collect_members(
    class_body: tree_sitter::Node,
    fqcn: &str,
    members: &mut Vec<IndexedMember>,
    uri: &lsp_types::Url,
    rope: &Rope,
) {
    let mut cursor = class_body.walk();
    for child in class_body.children(&mut cursor) {
        if child.kind() == "method_declaration" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let name = get_node_text(name_node, rope);
                let fqmn = format!("{}.{}", fqcn, name);
                members.push(IndexedMember {
                    name,
                    fqmn,
                    uri: uri.clone(),
                    range: node_range(name_node, rope),
                });
            }
        } else if child.kind() == "field_declaration" {
            let mut sub_cursor = child.walk();
            for sub in child.children(&mut sub_cursor) {
                if sub.kind() == "variable_declarator"
                    && let Some(name_node) = sub.child_by_field_name("name")
                {
                    let name = get_node_text(name_node, rope);
                    let fqmn = format!("{}.{}", fqcn, name);
                    members.push(IndexedMember {
                        name,
                        fqmn,
                        uri: uri.clone(),
                        range: node_range(name_node, rope),
                    });
                }
            }
        }
    }
}
