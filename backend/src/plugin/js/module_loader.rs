//! Resolve static and dynamic ESM imports exclusively inside a plugin package.
use anyhow::{Context, Result, bail};
use deno_core::{
    ModuleLoadResponse, ModuleLoader, ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType,
    RequestedModuleType, ResolutionKind,
};
use std::path::{Path, PathBuf};

pub struct PackageModuleLoader {
    root: PathBuf,
}

impl PackageModuleLoader {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: root
                .canonicalize()
                .context("invalid plugin package directory")?,
        })
    }

    fn checked_path(&self, specifier: &ModuleSpecifier) -> Result<PathBuf> {
        let path = specifier
            .to_file_path()
            .map_err(|_| anyhow::anyhow!("plugin imports must use local files"))?;
        if !matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("js" | "mjs")
        ) {
            bail!("plugin imports must use explicit .js or .mjs extensions");
        }
        let canonical = path
            .canonicalize()
            .context("plugin module does not exist")?;
        if !canonical.starts_with(&self.root) || !canonical.is_file() {
            bail!("plugin module escapes package directory");
        }
        Ok(canonical)
    }
}

impl ModuleLoader for PackageModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: ResolutionKind,
    ) -> Result<ModuleSpecifier> {
        let target = if kind == ResolutionKind::MainModule {
            ModuleSpecifier::parse(specifier).context("invalid plugin entry module URL")?
        } else {
            if !(specifier.starts_with("./") || specifier.starts_with("../")) {
                bail!("plugin imports must be package-relative .js paths");
            }
            let base = ModuleSpecifier::parse(referrer).context("invalid plugin referrer URL")?;
            self.checked_path(&base)?;
            base.join(specifier).context("invalid relative import")?
        };
        let path = self.checked_path(&target)?;
        ModuleSpecifier::from_file_path(path)
            .map_err(|_| anyhow::anyhow!("cannot form plugin module URL"))
    }

    fn load(
        &self,
        specifier: &ModuleSpecifier,
        _referrer: Option<&ModuleSpecifier>,
        _dynamic: bool,
        requested_type: RequestedModuleType,
    ) -> ModuleLoadResponse {
        ModuleLoadResponse::Sync((|| {
            if requested_type != RequestedModuleType::None {
                bail!("plugin imports may only load JavaScript modules");
            }
            let path = self.checked_path(specifier)?;
            let code = std::fs::read(&path)
                .with_context(|| format!("failed to read plugin module {}", path.display()))?;
            Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::Bytes(code.into_boxed_slice().into()),
                specifier,
            ))
        })())
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/plugin/js/module_loader.rs"]
mod tests;
