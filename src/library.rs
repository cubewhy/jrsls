use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use tower_lsp::lsp_types::{Location, Url};
use zip::ZipArchive;

pub trait SourceProvider: Send + Sync {
    fn fetch(&self, entry_path: &str) -> anyhow::Result<String>;
}

pub struct ZipSourceProvider {
    zip_path: PathBuf,
}

impl ZipSourceProvider {
    pub fn new(zip_path: PathBuf) -> Self {
        Self { zip_path }
    }
}

impl SourceProvider for ZipSourceProvider {
    fn fetch(&self, entry_path: &str) -> anyhow::Result<String> {
        let file = std::fs::File::open(&self.zip_path)?;
        let mut archive = ZipArchive::new(file)?;
        let mut zip_entry = archive.by_name(entry_path)?;

        let mut contents = String::new();
        zip_entry.read_to_string(&mut contents)?;
        Ok(contents)
    }
}

/// Keeps track of source providers keyed by URI scheme so we can
/// materialize virtual URIs (e.g. jrsls-std:///) into temp files
/// and hand them back to editors. Future providers (e.g. jar+decompiler)
/// can be registered without changing LSP flow.
#[derive(Default)]
pub struct SourceArchiveRegistry {
    providers: DashMap<String, Arc<dyn SourceProvider>>,
}

impl SourceArchiveRegistry {
    pub fn new() -> Self {
        Self {
            providers: DashMap::new(),
        }
    }

    pub fn register_zip(&self, scheme: &str, zip_path: PathBuf) {
        self.providers.insert(
            scheme.to_string(),
            Arc::new(ZipSourceProvider::new(zip_path)),
        );
    }

    pub fn materialize(&self, location: &Location) -> Option<Location> {
        let scheme = location.uri.scheme();
        let provider = self.providers.get(scheme)?;

        // Strip leading slash to avoid absolute path duplication in the temp dir
        let entry_path = location.uri.path().trim_start_matches('/');
        let contents = provider.fetch(entry_path).ok()?;

        let target_path = std::env::temp_dir()
            .join("jrsls")
            .join(scheme)
            .join(entry_path);

        if let Some(parent) = target_path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return None;
            }
        }
        if std::fs::write(&target_path, contents).is_err() {
            return None;
        }

        let uri = Url::from_file_path(&target_path).ok()?;

        let mut new_loc = location.clone();
        new_loc.uri = uri;
        Some(new_loc)
    }
}
