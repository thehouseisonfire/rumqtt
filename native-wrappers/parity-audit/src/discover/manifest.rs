use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
struct Manifest {
    package: Option<Package>,
    lib: Option<Library>,
}

#[derive(Deserialize)]
struct Package {
    autolib: Option<bool>,
}

#[derive(Deserialize)]
struct Library {
    path: Option<PathBuf>,
}

/// Read only Cargo's library-selection inputs. Other targets, dependencies and
/// workspace inheritance do not need resolution to select the library source.
pub(super) fn library_source(manifest: &Path) -> Result<PathBuf, String> {
    let source = std::fs::read_to_string(manifest)
        .map_err(|error| format!("{}: {error}", manifest.display()))?;
    let parsed: Manifest = toml::from_str(&source)
        .map_err(|error| format!("invalid manifest {}: {error}", manifest.display()))?;
    let package = parsed
        .package
        .ok_or_else(|| format!("no package library target in {}", manifest.display()))?;
    if parsed.lib.is_none() && package.autolib == Some(false) {
        return Err(format!(
            "no library target in {}: package.autolib is false and [lib] is absent",
            manifest.display()
        ));
    }
    let selected = parsed
        .lib
        .and_then(|lib| lib.path)
        .unwrap_or_else(|| "src/lib.rs".into());
    let path = manifest.parent().unwrap().join(selected);
    if !path.is_file() {
        return Err(format!(
            "missing library source {} selected by {}",
            path.display(),
            manifest.display()
        ));
    }
    Ok(path)
}
