use dashmap::DashMap;
use ropey::Rope;
use tower_lsp::lsp_types;
use tree_sitter::Tree;

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub package_name: Option<String>,
    pub imports: Vec<String>,
    pub defined_classes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ClassLocation {
    pub fqcn: String,
    pub uri: lsp_types::Url,
    pub range: lsp_types::Range,
}

pub struct GlobalIndex {
    pub short_name_map: DashMap<String, Vec<ClassLocation>>,
    pub file_info: DashMap<String, FileInfo>,
}

impl GlobalIndex {
    pub fn new() -> Self {
        Self {
            short_name_map: DashMap::new(),
            file_info: DashMap::new(),
        }
    }
}

impl Default for GlobalIndex {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Document {
    pub text: Rope,
    pub tree: Tree,
}
