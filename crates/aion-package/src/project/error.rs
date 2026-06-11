//! Error taxonomy for project-level workflow packaging.

use std::path::PathBuf;

use crate::PackageError;

/// Errors produced while packaging a built Gleam workflow project.
///
/// Every variant carries the offending path, field, or module as structured
/// data — not just formatted text — so callers can map variants to actionable
/// guidance (for example, [`PackagingError::ProjectNotBuilt`] maps to "run
/// `gleam build`").
#[derive(thiserror::Error, Debug)]
pub enum PackagingError {
    /// The project root contains no `workflow.toml` packaging descriptor.
    #[error("no workflow.toml found in {root}")]
    ConfigMissing {
        /// Project root that was searched for the descriptor.
        root: PathBuf,
    },

    /// The `workflow.toml` descriptor exists but could not be read.
    #[error("failed to read {path}: {source}")]
    ConfigRead {
        /// Path of the descriptor that could not be read.
        path: PathBuf,
        /// I/O failure reported while reading the descriptor.
        source: std::io::Error,
    },

    /// The `workflow.toml` descriptor is not valid TOML for the schema.
    ///
    /// The wrapped error carries unknown-key detail: every table rejects
    /// unrecognised keys, naming the key and its location.
    #[error("failed to parse {path}: {source}")]
    ConfigParse {
        /// Path of the descriptor that failed to parse.
        path: PathBuf,
        /// TOML deserialisation failure, including unknown-key detail.
        source: toml::de::Error,
    },

    /// The `workflow.toml` descriptor parsed but failed semantic validation.
    #[error("invalid workflow.toml: {field}: {reason}")]
    ConfigInvalid {
        /// Descriptor field that failed validation, e.g. `workflow[0].entry_module`.
        field: String,
        /// Human-readable reason the field value was rejected.
        reason: String,
    },

    /// A declared JSON-Schema file could not be read.
    #[error("failed to read schema {path}: {source}")]
    SchemaRead {
        /// Resolved path of the schema file that could not be read.
        path: PathBuf,
        /// I/O failure reported while reading the schema file.
        source: std::io::Error,
    },

    /// A declared JSON-Schema file is not valid JSON.
    #[error("schema {path} is not valid JSON: {source}")]
    SchemaParse {
        /// Resolved path of the schema file that failed to parse.
        path: PathBuf,
        /// JSON parsing failure reported by `serde_json`.
        source: serde_json::Error,
    },

    /// The compiled Erlang output required for packaging does not exist.
    #[error("project is not built: {missing} does not exist; run `gleam build` first")]
    ProjectNotBuilt {
        /// Build-output path that was required but absent.
        missing: PathBuf,
    },

    /// The project root contains no `gleam.toml`, so it is not a Gleam project.
    #[error("not a Gleam project: {path} not found")]
    GleamTomlMissing {
        /// Path where `gleam.toml` was expected.
        path: PathBuf,
    },

    /// A Gleam metadata file (`gleam.toml` or the `manifest.toml` lockfile)
    /// could not be read.
    #[error("failed to read Gleam metadata {path}: {source}")]
    GleamMetadataRead {
        /// Path of the metadata file that could not be read.
        path: PathBuf,
        /// I/O failure reported while reading the metadata file.
        source: std::io::Error,
    },

    /// A Gleam metadata file could not be parsed as the expected TOML shape.
    #[error("failed to parse Gleam metadata {path}: {source}")]
    GleamMetadataParse {
        /// Path of the metadata file that failed to parse.
        path: PathBuf,
        /// TOML deserialisation failure reported while parsing the file.
        source: toml::de::Error,
    },

    /// A production dependency named in `gleam.toml` is absent from the
    /// `manifest.toml` lockfile, so the dependency closure cannot be computed.
    #[error("dependency `{package}` is in gleam.toml but missing from manifest.toml; rebuild")]
    DependencyUnresolved {
        /// Gleam package name that could not be resolved in the lockfile.
        package: String,
    },

    /// A compiled `.beam` module (or its containing directory) could not be read.
    #[error("failed to read compiled module {path}: {source}")]
    BeamRead {
        /// Path of the compiled module or module directory that failed to read.
        path: PathBuf,
        /// I/O failure reported while reading the compiled output.
        source: std::io::Error,
    },

    /// A compiled module filename is not valid UTF-8 and cannot become a
    /// logical module name.
    #[error("compiled module filename is not valid UTF-8: {path}")]
    ModuleNameNotUtf8 {
        /// Path whose filename failed UTF-8 decoding.
        path: PathBuf,
    },

    /// Two Gleam packages in the production dependency closure provide the same
    /// logical module name.
    #[error("module `{module}` is provided by both `{first}` and `{second}`")]
    DuplicateModule {
        /// Logical module name provided more than once.
        module: String,
        /// Gleam package that provided the module first.
        first: String,
        /// Gleam package that provided the module again.
        second: String,
    },

    /// A declared workflow entry module is absent from the compiled output.
    #[error("entry module `{module}` not found in compiled output under {searched}")]
    EntryModuleNotFound {
        /// Entry module declared by `workflow.toml`.
        module: String,
        /// Compiled-output directory that was searched.
        searched: PathBuf,
    },

    /// A first-party Gleam source file could not be read for inclusion.
    #[error("failed to read source file {path}: {source}")]
    SourceRead {
        /// Path of the source file or directory that could not be read.
        path: PathBuf,
        /// I/O failure reported while reading the source tree.
        source: std::io::Error,
    },

    /// A `workflow.toml`-declared path is absolute or escapes the project
    /// root after lexically folding `.` and `..` components.
    ///
    /// Only descriptor-sourced paths (`output`, `input_schema`,
    /// `output_schema`) are confined to the root; the programmatic
    /// [`PackageOptions::output_override`](crate::PackageOptions) is the
    /// caller's own path and is intentionally exempt.
    #[error("invalid workflow.toml: {field}: path {path} is absolute or escapes the project root")]
    PathEscapesRoot {
        /// Descriptor field that declared the path, e.g. `workflow[0].output`.
        field: String,
        /// The offending path exactly as declared in the descriptor.
        path: PathBuf,
    },

    /// Two workflows resolve to the same output archive path.
    #[error("workflows `{first}` and `{second}` both write to {path}")]
    OutputConflict {
        /// Entry module of the workflow that claimed the path first.
        first: String,
        /// Entry module of the workflow that claimed the path again.
        second: String,
        /// Output path claimed by both workflows.
        path: PathBuf,
    },

    /// An output override was supplied for a project declaring multiple workflows.
    #[error("--out is only valid for single-workflow projects ({count} declared)")]
    OutputOverrideAmbiguous {
        /// Number of workflows the project declares.
        count: usize,
    },

    /// A workflow's `.aion` archive could not be built or written to its
    /// resolved output path.
    ///
    /// Unlike the transparent [`PackagingError::Package`] variant, this one
    /// names the output path, so "No such file or directory"-style I/O
    /// failures identify the file that could not be written.
    #[error("failed to write archive {path}: {source}")]
    OutputWrite {
        /// Resolved output path the archive could not be written to.
        path: PathBuf,
        /// Package-format failure reported while building or writing.
        source: PackageError,
    },

    /// A package-format failure surfaced while building, writing, or re-loading
    /// an archive (reserved module names, write I/O, verify-after-write).
    #[error(transparent)]
    Package(#[from] PackageError),
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::PackagingError;

    fn assert_send_sync<T: Send + Sync + 'static>() {}

    #[test]
    fn packaging_error_is_send_sync_and_static() {
        assert_send_sync::<PackagingError>();
    }

    #[test]
    fn display_messages_name_the_failed_condition() {
        assert_eq!(
            PackagingError::ConfigMissing {
                root: PathBuf::from("/project"),
            }
            .to_string(),
            "no workflow.toml found in /project"
        );
        assert_eq!(
            PackagingError::ConfigInvalid {
                field: "workflow[0].timeout_seconds".to_owned(),
                reason: "must be at least 1".to_owned(),
            }
            .to_string(),
            "invalid workflow.toml: workflow[0].timeout_seconds: must be at least 1"
        );
        assert_eq!(
            PackagingError::ProjectNotBuilt {
                missing: PathBuf::from("/project/build/dev/erlang"),
            }
            .to_string(),
            "project is not built: /project/build/dev/erlang does not exist; \
             run `gleam build` first"
        );
        assert_eq!(
            PackagingError::DependencyUnresolved {
                package: "gleam_json".to_owned(),
            }
            .to_string(),
            "dependency `gleam_json` is in gleam.toml but missing from manifest.toml; rebuild"
        );
        assert_eq!(
            PackagingError::DuplicateModule {
                module: "shared".to_owned(),
                first: "pkg_a".to_owned(),
                second: "pkg_b".to_owned(),
            }
            .to_string(),
            "module `shared` is provided by both `pkg_a` and `pkg_b`"
        );
        assert_eq!(
            PackagingError::EntryModuleNotFound {
                module: "ghost".to_owned(),
                searched: PathBuf::from("/project/build/dev/erlang"),
            }
            .to_string(),
            "entry module `ghost` not found in compiled output under /project/build/dev/erlang"
        );
        assert_eq!(
            PackagingError::OutputConflict {
                first: "alpha".to_owned(),
                second: "beta".to_owned(),
                path: PathBuf::from("/project/alpha.aion"),
            }
            .to_string(),
            "workflows `alpha` and `beta` both write to /project/alpha.aion"
        );
        assert_eq!(
            PackagingError::OutputOverrideAmbiguous { count: 3 }.to_string(),
            "--out is only valid for single-workflow projects (3 declared)"
        );
        assert_eq!(
            PackagingError::PathEscapesRoot {
                field: "workflow[0].output".to_owned(),
                path: PathBuf::from("../escape.aion"),
            }
            .to_string(),
            "invalid workflow.toml: workflow[0].output: path ../escape.aion \
             is absolute or escapes the project root"
        );
        let output_write = PackagingError::OutputWrite {
            path: PathBuf::from("/project/missing/demo.aion"),
            source: crate::PackageError::ArchiveWriteIo {
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            },
        };
        assert!(
            output_write
                .to_string()
                .starts_with("failed to write archive /project/missing/demo.aion: ")
        );
        assert!(std::error::Error::source(&output_write).is_some());
    }

    #[test]
    fn package_error_converts_transparently() {
        let error = PackagingError::from(crate::PackageError::MissingManifest);

        assert_eq!(error.to_string(), "missing required manifest.json entry");
        assert!(matches!(error, PackagingError::Package(_)));
    }
}
