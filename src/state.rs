use std::sync::Mutex;

use dashmap::{DashMap, mapref::entry::Entry};
use ropey::Rope;
use salsa::Setter;
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

#[derive(Debug, Clone)]
pub struct MemberLocation {
    pub fqmn: String,
    pub uri: lsp_types::Url,
    pub range: lsp_types::Range,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct IndexedClass {
    pub short_name: String,
    pub fqcn: String,
    pub uri: lsp_types::Url,
    pub range: lsp_types::Range,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct IndexedMember {
    pub name: String,
    pub fqmn: String,
    pub uri: lsp_types::Url,
    pub range: lsp_types::Range,
}

#[salsa::input]
struct FileIndex {
    uri: String,
    package_name: Option<String>,
    imports: Vec<String>,
    classes: Vec<IndexedClass>,
    members: Vec<IndexedMember>,
}

#[salsa::db]
#[derive(Clone, Default)]
struct IndexStorage {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for IndexStorage {}

pub struct GlobalIndex {
    storage: Mutex<IndexStorage>,
    handles: DashMap<String, FileIndex>,
}

impl GlobalIndex {
    pub fn new() -> Self {
        Self {
            storage: Mutex::new(IndexStorage::default()),
            handles: DashMap::new(),
        }
    }

    pub fn upsert_file(
        &self,
        uri: &str,
        package_name: Option<String>,
        imports: Vec<String>,
        classes: Vec<IndexedClass>,
        members: Vec<IndexedMember>,
    ) {
        let mut db = self
            .storage
            .lock()
            .expect("GlobalIndex storage poisoned unexpectedly");

        match self.handles.entry(uri.to_string()) {
            Entry::Occupied(entry) => {
                let handle = entry.get();
                handle.set_package_name(&mut *db).to(package_name);
                handle.set_imports(&mut *db).to(imports);
                handle.set_classes(&mut *db).to(classes);
                handle.set_members(&mut *db).to(members);
            }
            Entry::Vacant(entry) => {
                entry.insert(FileIndex::new(
                    &mut *db,
                    uri.to_string(),
                    package_name,
                    imports,
                    classes,
                    members,
                ));
            }
        }
    }

    pub fn file_info(&self, uri: &str) -> Option<FileInfo> {
        let db = self.storage.lock().ok()?;
        let handle = self.handles.get(uri)?;

        let imports = handle.imports(&*db);
        let classes = handle.classes(&*db);

        Some(FileInfo {
            package_name: handle.package_name(&*db),
            imports,
            defined_classes: classes.iter().map(|c| c.short_name.clone()).collect(),
        })
    }

    pub fn classes_by_short_name(&self, short_name: &str) -> Vec<ClassLocation> {
        let db = match self.storage.lock() {
            Ok(db) => db,
            Err(_) => return Vec::new(),
        };

        self.handles
            .iter()
            .flat_map(|entry| {
                entry
                    .value()
                    .classes(&*db)
                    .into_iter()
                    .filter(move |class| class.short_name == short_name)
                    .map(|class| ClassLocation {
                        fqcn: class.fqcn.clone(),
                        uri: class.uri.clone(),
                        range: class.range,
                    })
            })
            .collect()
    }

    pub fn members_by_name(&self, name: &str) -> Vec<MemberLocation> {
        let db = match self.storage.lock() {
            Ok(db) => db,
            Err(_) => return Vec::new(),
        };

        self.handles
            .iter()
            .flat_map(|entry| {
                entry
                    .value()
                    .members(&*db)
                    .into_iter()
                    .filter(move |member| member.name == name)
                    .map(|member| MemberLocation {
                        fqmn: member.fqmn.clone(),
                        uri: member.uri.clone(),
                        range: member.range,
                    })
            })
            .collect()
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
