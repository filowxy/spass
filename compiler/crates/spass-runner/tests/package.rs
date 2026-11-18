//! Tests reading `.saple` packages.

use std::io::Write;
use std::path::{Path, PathBuf};

use spass_runner::Saple;

/// The name of the manifest inside a `.saple`.
const MANIFEST_NAME: &str = "manifest.json";

/// A directory the tests may write into.
fn test_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
}

/// Write a `.saple` with the given manifest and native entries.
fn write_package(path: &Path, manifest: Option<&str>, natives: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    if let Some(manifest) = manifest {
        zip.start_file(MANIFEST_NAME, options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
    }
    for (name, bytes) in natives {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn a_package_round_trips() {
    let path = test_dir().join("round-trip.saple");
    write_package(
        &path,
        Some(r#"{"module":"spass_module","version":"0.1.0"}"#),
        &[("native/windows_x64.dll", b"not really a library")],
    );

    let package = Saple::read(&path).unwrap();
    assert_eq!(package.manifest["module"], "spass_module");
    assert_eq!(package.manifest["version"], "0.1.0");

    let native = package
        .native_for("windows_x64")
        .expect("the package carries a windows_x64 module");
    assert_eq!(native.entry, "native/windows_x64.dll");
    assert_eq!(native.bytes, b"not really a library".to_vec());
    assert!(package.native_for("linux_x64").is_none());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_package_without_a_manifest_is_an_error() {
    let path = test_dir().join("no-manifest.saple");
    write_package(
        &path,
        None,
        &[("native/windows_x64.dll", b"not really a library")],
    );

    let error = Saple::read(&path).unwrap_err();
    assert!(
        format!("{:#}", error).contains(MANIFEST_NAME),
        "the error has to name the missing manifest: {:#}",
        error
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_current_module_is_extracted_under_its_own_name() {
    let platform = spass_runner::platform_tag();
    let entry = spass_runner::native_entry_name(&platform);
    let path = test_dir().join("extract.saple");
    write_package(
        &path,
        Some(r#"{"module":"spass_module"}"#),
        &[(entry.as_str(), b"not really a library")],
    );
    let package = Saple::read(&path).unwrap();

    let dir = test_dir().join("extract-cache");
    let written = package.extract_current(&dir).unwrap();
    let file_name = entry.rsplit('/').next().unwrap();
    assert_eq!(written, dir.join(file_name));
    assert_eq!(std::fs::read(&written).unwrap(), b"not really a library".to_vec());

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&dir);
}
