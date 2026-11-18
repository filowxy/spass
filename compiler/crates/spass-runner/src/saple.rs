//! Reads `.saple` packages.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::platform_tag;

/// The name of the manifest inside a `.saple`.
pub const MANIFEST_NAME: &str = "manifest.json";

/// The directory the native modules live in inside a `.saple`.
pub const NATIVE_DIR: &str = "native/";

/// The native module a `.saple` carries for one platform.
#[derive(Debug, Clone)]
pub struct Native {
    /// The platform tag, e.g. `windows_x64`.
    pub platform: String,
    /// The name of the entry inside the ZIP, e.g. `native/windows_x64.dll`.
    pub entry: String,
    /// The bytes of the entry.
    pub bytes: Vec<u8>,
}

/// A `.saple` package: a ZIP container.
#[derive(Debug, Clone)]
pub struct Saple {
    /// The package manifest (`manifest.json` inside the ZIP).
    pub manifest: serde_json::Value,
    /// The native modules the package carries.
    pub natives: Vec<Native>,
}

impl Saple {
    /// Read a `.saple` from disk.
    pub fn read(path: &Path) -> Result<Saple> {
        let file =
            std::fs::File::open(path).with_context(|| format!("opening the package {}", path.display()))?;
        let mut archive = zip::ZipArchive::new(file)
            .with_context(|| format!("reading the package {}", path.display()))?;

        let mut manifest = None;
        let mut natives = Vec::new();
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .with_context(|| format!("reading entry {} of {}", index, path.display()))?;
            let name = entry.name().to_string();
            if entry.is_dir() {
                continue;
            }
            if name == MANIFEST_NAME {
                let mut text = String::new();
                entry
                    .read_to_string(&mut text)
                    .with_context(|| format!("reading {} of {}", MANIFEST_NAME, path.display()))?;
                let value = serde_json::from_str(&text)
                    .with_context(|| format!("parsing {} of {}", MANIFEST_NAME, path.display()))?;
                manifest = Some(value);
                continue;
            }
            let Some(relative) = name.strip_prefix(NATIVE_DIR) else {
                continue;
            };
            if relative.is_empty() {
                continue;
            }
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .with_context(|| format!("reading {} of {}", name, path.display()))?;
            natives.push(Native {
                platform: platform_of(relative),
                entry: name,
                bytes,
            });
        }

        let manifest = manifest.ok_or_else(|| {
            anyhow::anyhow!("the package {} has no {}", path.display(), MANIFEST_NAME)
        })?;
        Ok(Saple { manifest, natives })
    }

    /// The native module for one platform.
    pub fn native_for(&self, platform: &str) -> Option<&Native> {
        self.natives.iter().find(|native| native.platform == platform)
    }

    /// The native module for the platform this process runs on.
    pub fn native_for_current_platform(&self) -> Option<&Native> {
        self.native_for(&platform_tag())
    }

    /// Write the native module for the current platform into `dir` and
    /// return the path of the written file.
    ///
    /// The file name inside `dir` is the base name of the ZIP entry, so
    /// the loader sees the same file name the package advertises.
    pub fn extract_current(&self, dir: &Path) -> Result<PathBuf> {
        let native = self.native_for_current_platform().ok_or_else(|| {
            anyhow::anyhow!(
                "the package carries no native module for {} (it carries {})",
                platform_tag(),
                self.platforms().join(", ")
            )
        })?;
        let file_name = native.entry.rsplit('/').next().unwrap_or(&native.entry);
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating the cache directory {}", dir.display()))?;
        let path = dir.join(file_name);
        std::fs::write(&path, &native.bytes)
            .with_context(|| format!("writing the native module {}", path.display()))?;
        Ok(path)
    }

    /// The platform tags the package carries.
    fn platforms(&self) -> Vec<&str> {
        self.natives
            .iter()
            .map(|native| native.platform.as_str())
            .collect()
    }
}

/// The platform tag of a native entry.
///
/// The tag is the file stem: the part of the file name before the
/// first dot. A package that carries `native/windows_x64.dll` is a
/// package for `windows_x64`.
fn platform_of(entry: &str) -> String {
    let file_name = entry.rsplit('/').next().unwrap_or(entry);
    match file_name.split_once('.') {
        Some((stem, _)) => stem.to_string(),
        None => file_name.to_string(),
    }
}
