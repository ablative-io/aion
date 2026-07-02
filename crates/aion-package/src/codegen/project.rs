//! Project-level codegen plumbing shared by every generator: the write/check
//! mode, the byte-exact `--check` comparison, and the package-name read.
//!
//! The schema-first front door (`codegen_project`, which read authored
//! `schemas/*.json`) is gone: the authored source of truth is the Gleam types
//! module `src/<package>_io.gleam` (ADR-014, resolved types-first 2026-07-02),
//! mapped into the model by [`super::interface`], and `schemas/*.json` is now
//! an emitted artifact owned by [`super::schema_emit`].

use std::io;
use std::path::Path;

use serde::Deserialize;

use super::error::CodegenError;
use super::names::{is_reserved_word, is_snake_identifier};
use crate::PackagingError;

/// What to do with generated output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodegenMode {
    /// Write every generated file, replacing existing ones.
    Write,
    /// Compare against the on-disk files and fail on drift without writing
    /// (CI gate).
    Check,
}

/// Byte-compares a generated file's fresh contents against the on-disk file,
/// failing with a typed error when the file is missing, unreadable, or
/// drifted.
pub(crate) fn check_on_disk(module_path: &Path, contents: &str) -> Result<(), CodegenError> {
    let on_disk = match std::fs::read(module_path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(CodegenError::CheckMissing {
                path: module_path.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(CodegenError::CheckRead {
                path: module_path.to_path_buf(),
                source,
            });
        }
    };
    if on_disk != contents.as_bytes() {
        return Err(CodegenError::CheckDrift {
            path: module_path.to_path_buf(),
        });
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GleamTomlName {
    name: String,
}

/// Reads the Gleam package name from `<root>/gleam.toml`; it prefixes every
/// generated module (`src/<name>_codecs.gleam`) and names the authored types
/// module (`src/<name>_io.gleam`).
pub(crate) fn read_package_name(root: &Path) -> Result<String, CodegenError> {
    let path = root.join("gleam.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(CodegenError::Config(PackagingError::GleamTomlMissing {
                path,
            }));
        }
        Err(source) => {
            return Err(CodegenError::Config(PackagingError::GleamMetadataRead {
                path,
                source,
            }));
        }
    };
    let parsed: GleamTomlName = toml::from_str(&text).map_err(|source| {
        CodegenError::Config(PackagingError::GleamMetadataParse { path, source })
    })?;
    if !is_snake_identifier(&parsed.name) || is_reserved_word(&parsed.name) {
        return Err(CodegenError::ProjectName {
            name: parsed.name,
            reason: "must be a snake_case identifier and not a Gleam reserved word".to_owned(),
        });
    }
    Ok(parsed.name)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{check_on_disk, read_package_name};
    use crate::PackagingError;
    use crate::codegen::error::CodegenError;
    use crate::project::fixture;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn check_on_disk_passes_clean_and_types_missing_and_drift() -> TestResult {
        let root = fixture::temp_project(
            "codegen-check-on-disk",
            &[("src/demo_codecs.gleam", b"generated contents\n" as &[u8])],
        )?;
        let path = root.join("src/demo_codecs.gleam");

        check_on_disk(&path, "generated contents\n")?;

        let drift = check_on_disk(&path, "other contents\n");
        assert!(matches!(
            drift,
            Err(CodegenError::CheckDrift { path: ref drifted }) if *drifted == path
        ));

        let missing_path = root.join("src/absent.gleam");
        let missing = check_on_disk(&missing_path, "anything");
        assert!(matches!(
            missing,
            Err(CodegenError::CheckMissing { path: ref reported }) if *reported == missing_path
        ));
        fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[test]
    fn package_name_reads_and_validates() -> TestResult {
        let root = fixture::temp_project(
            "codegen-package-name",
            &[(
                "gleam.toml",
                b"name = \"demo\"\nversion = \"0.1.0\"\n" as &[u8],
            )],
        )?;
        assert_eq!(read_package_name(&root)?, "demo");
        fs::remove_dir_all(&root)?;

        let missing = fixture::temp_project("codegen-package-name-missing", &[])?;
        let result = read_package_name(&missing);
        assert!(matches!(
            result,
            Err(CodegenError::Config(
                PackagingError::GleamTomlMissing { .. }
            ))
        ));
        fs::remove_dir_all(&missing)?;

        let bad = fixture::temp_project(
            "codegen-package-name-bad",
            &[("gleam.toml", b"name = \"Demo-App\"\n" as &[u8])],
        )?;
        let result = read_package_name(&bad);
        assert!(matches!(
            result,
            Err(CodegenError::ProjectName { ref name, .. }) if name == "Demo-App"
        ));
        fs::remove_dir_all(&bad)?;
        Ok(())
    }
}
