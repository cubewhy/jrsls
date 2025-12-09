use crate::state::FileInfo;
use crate::utils::get_node_text;
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

        let url = match lsp_types::Url::parse(uri) {
            Ok(u) => u,
            Err(_) => return,
        };

        while let Some((m, capture_idx_ref)) = capture_iter.next() {
            let capture_index = *capture_idx_ref;

            let capture = m.captures[capture_index];
            let node = capture.node;

            // 我们依然需要 rope 来获取文本
            let text = get_node_text(node, rope);

            let query_capture_idx = capture.index as usize;
            let capture_name = JAVA_QUERY.capture_names()[query_capture_idx];

            match capture_name {
                "package" => package_name = Some(text),
                "import" => imports.push(text),
                "class" | "interface" | "enum" | "record" => {
                    defined_classes.push(text.clone());

                    let fqcn = if let Some(pkg) = &package_name {
                        format!("{}.{}", pkg, text)
                    } else {
                        text.clone()
                    };

                    index
                        .short_name_map
                        .entry(text)
                        .or_default()
                        .push((fqcn, url.clone()));
                }
                _ => {}
            }
        }

        index.file_info.insert(
            uri.to_string(),
            FileInfo {
                package_name,
                imports,
                defined_classes: defined_classes.clone(),
            },
        );

        tracing::debug!("Indexed {}: classes={:?}", uri, defined_classes);
    }
}
