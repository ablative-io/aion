//! Error taxonomy for malformed `.aion` packages.

/// Errors produced while validating or loading a `.aion` package.
#[derive(thiserror::Error, Debug)]
pub enum PackageError {
    /// The archive could not be read as a ZIP container.
    #[error("failed to read .aion ZIP archive: {0}")]
    ArchiveRead(#[from] zip::result::ZipError),

    /// The archive does not contain the required root manifest.
    #[error("missing required manifest.json entry")]
    MissingManifest,

    /// A module uses a namespace owned by the engine's native NIF layer.
    #[error(
        "module `{module}` uses an engine-reserved namespace and must not ship as package bytecode"
    )]
    ReservedModuleName {
        /// The offending logical module name.
        module: String,
    },

    /// The archive could not be written as a ZIP container.
    #[error("failed to write .aion ZIP archive: {0}")]
    ArchiveWrite(zip::result::ZipError),

    /// The archive target could not be written to the filesystem or memory buffer.
    #[error("failed to write .aion archive bytes: {source}")]
    ArchiveWriteIo {
        /// I/O failure reported by the write target.
        source: std::io::Error,
    },

    /// The manifest entry is present but is not valid manifest JSON.
    #[error("failed to parse manifest.json: {source}")]
    ManifestParse {
        /// JSON parsing failure reported by `serde_json`.
        source: serde_json::Error,
    },

    /// The manifest could not be serialised for writing into the archive.
    #[error("failed to serialise manifest.json: {source}")]
    ManifestSerialise {
        /// JSON serialisation failure reported by `serde_json`.
        source: serde_json::Error,
    },

    /// The manifest declares a format version this crate does not support.
    #[error("unknown .aion format_version {found}")]
    UnknownFormatVersion {
        /// Unsupported format version found in the manifest.
        found: u32,
    },

    /// The manifest entry module is not present in the beam set.
    #[error("missing entry module `{module}` in beam set")]
    MissingEntryModule {
        /// Logical entry module named by the manifest.
        module: String,
    },

    /// The manifest version does not match the hash recomputed from beams.
    #[error("package integrity mismatch: expected version `{expected}`, computed `{computed}`")]
    IntegrityMismatch {
        /// Version claimed by the manifest.
        expected: String,
        /// Version recomputed from package beams.
        computed: String,
    },

    /// A beam archive entry is malformed or ambiguous.
    #[error("malformed beam entry `{entry}`")]
    MalformedBeamEntry {
        /// Archive entry or logical module name that failed validation.
        entry: String,
    },

    /// The archive's entries inflate past the caller's extraction budget.
    #[error(
        "archive contents inflate past the extraction limit of {limit} bytes; refusing to extract further"
    )]
    InflatedSizeExceeded {
        /// The caller-configured inflate ceiling in bytes.
        limit: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::PackageError;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn package_error_is_send_and_sync() {
        assert_send_sync::<PackageError>();
    }

    #[test]
    fn display_messages_name_the_failed_condition() {
        assert_eq!(
            PackageError::MissingManifest.to_string(),
            "missing required manifest.json entry"
        );
        assert_eq!(
            PackageError::ArchiveWriteIo {
                source: std::io::Error::other("disk full"),
            }
            .to_string(),
            "failed to write .aion archive bytes: disk full"
        );
        assert_eq!(
            PackageError::UnknownFormatVersion { found: 99 }.to_string(),
            "unknown .aion format_version 99"
        );
        assert_eq!(
            PackageError::MissingEntryModule {
                module: "workflow/main".to_owned(),
            }
            .to_string(),
            "missing entry module `workflow/main` in beam set"
        );
        assert_eq!(
            PackageError::IntegrityMismatch {
                expected: "expected".to_owned(),
                computed: "computed".to_owned(),
            }
            .to_string(),
            "package integrity mismatch: expected version `expected`, computed `computed`"
        );
        assert_eq!(
            PackageError::MalformedBeamEntry {
                entry: "beam/workflow.beam".to_owned(),
            }
            .to_string(),
            "malformed beam entry `beam/workflow.beam`"
        );
        assert_eq!(
            PackageError::InflatedSizeExceeded { limit: 1024 }.to_string(),
            "archive contents inflate past the extraction limit of 1024 bytes; refusing to extract further"
        );
    }
}
