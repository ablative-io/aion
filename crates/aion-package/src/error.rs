//! Error taxonomy for malformed `.aion` packages.

/// Errors produced while validating or loading a `.aion` package.
#[derive(thiserror::Error, Debug)]
pub enum PackageError {
    /// The archive could not be read as a ZIP container.
    #[error("failed to read .aion ZIP archive")]
    ArchiveRead(#[from] zip::result::ZipError),

    /// The archive does not contain the required root manifest.
    #[error("missing required manifest.json entry")]
    MissingManifest,

    /// The manifest entry is present but is not valid manifest JSON.
    #[error("failed to parse manifest.json")]
    ManifestParse {
        /// JSON parsing failure reported by `serde_json`.
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
}
