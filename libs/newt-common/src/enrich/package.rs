//! Package enricher: which directories of a listing macOS presents as
//! files (apps, bundles, `.rtfd` documents…), as the filesystem says
//! (`Vfs::is_package`). Automatic. Only a filesystem served by a macOS
//! kernel can say yes, so elsewhere it returns at once.

use super::{
    Annotation, EnrichScope, EnrichSink, Enricher, EnricherDescriptor, RegisteredEnricher,
};
use crate::Error;
use crate::vfs::{VfsDescriptor, VfsPath, VfsRegistry};

pub struct PackageEnricherDescriptor;

pub static PACKAGE_ENRICHER_DESCRIPTOR: PackageEnricherDescriptor = PackageEnricherDescriptor;
inventory::submit! { RegisteredEnricher(&PACKAGE_ENRICHER_DESCRIPTOR) }

impl EnricherDescriptor for PackageEnricherDescriptor {
    fn id(&self) -> &'static str {
        "package"
    }

    fn activity(&self) -> &'static str {
        "Finding packages"
    }

    fn automatic(&self) -> bool {
        true
    }

    fn applies_to_vfs(&self, vfs: &dyn VfsDescriptor) -> bool {
        vfs.type_name() == "local"
    }
}

pub struct PackageEnricher;

#[async_trait::async_trait]
impl Enricher for PackageEnricher {
    fn descriptor(&self) -> &'static dyn EnricherDescriptor {
        &PACKAGE_ENRICHER_DESCRIPTOR
    }

    async fn enrich(
        &self,
        registry: &VfsRegistry,
        path: &VfsPath,
        scope: &EnrichScope,
        sink: &EnrichSink,
    ) -> Result<(), Error> {
        if !cfg!(target_os = "macos") {
            return Ok(());
        }
        let (vfs, dir) = registry.resolve(path)?;
        let listing = vfs.list_files(&dir, None).await?;
        for file in listing.files.iter().filter(|f| {
            f.name != ".."
                && f.is_dir
                && !f.is_symlink
                && match scope {
                    EnrichScope::AllEntries => true,
                    EnrichScope::Entries(keys) => keys.iter().any(|k| k == f.key()),
                }
        }) {
            if vfs.is_package(&dir.join(&file.name)).await.unwrap_or(false) {
                sink.emit_entry(file.key().to_string(), Annotation::Package {});
            }
        }
        Ok(())
    }
}
