//! Error taxonomy for structure extraction and bounded Gleam regeneration.
//!
//! Every variant carries the offending module name, identifier, or reason as
//! structured data so a consumer can render actionable guidance. There are no
//! silent empty graphs: a missing or unreadable entry source, an unknown
//! activity, an out-of-bounds delta, or an unsafe regenerated name is always a
//! loud, typed failure.

/// Errors produced while extracting a workflow graph model from a package or
/// regenerating Gleam from a bounded structural delta.
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum StructureError {
    /// The manifest names an entry module whose Gleam source is absent from the
    /// package `src/` set, so the primitive structure cannot be derived.
    ///
    /// A package may legitimately ship without source (beam-only deploys), but
    /// structure extraction requires the verbatim entry-module source to read.
    #[error(
        "entry module `{module}` has no Gleam source in the package; structure extraction \
         requires the verbatim entry-module source"
    )]
    MissingEntrySource {
        /// Logical entry-module name named by the manifest.
        module: String,
    },

    /// The entry-module source bytes are not valid UTF-8 and cannot be scanned
    /// for the workflow vocabulary.
    #[error("entry module `{module}` source is not valid UTF-8")]
    EntrySourceNotUtf8 {
        /// Logical entry-module name whose bytes failed to decode.
        module: String,
    },

    /// The entry-module source never imports `aion/workflow`, so it invokes
    /// none of the recorded primitives and has no extractable structure.
    ///
    /// This is reported rather than returning an empty graph, so a workflow the
    /// extractor cannot understand fails loudly instead of rendering as blank.
    #[error(
        "entry module `{module}` does not import `aion/workflow`; the extractor only \
         understands workflows that compose the `aion/workflow` primitive vocabulary"
    )]
    NoWorkflowImport {
        /// Logical entry-module name that lacked the import.
        module: String,
    },

    /// A `run` node names an activity the package manifest does not declare, so
    /// the extracted graph would not correspond to the workflow's real
    /// activities.
    #[error(
        "extracted `run` node references activity `{activity}`, which the manifest does not \
         declare; the graph would not match the workflow's recorded activities"
    )]
    UnknownActivity {
        /// The activity name found in source but absent from the manifest.
        activity: String,
    },

    /// A structural delta fell outside the bounded round-trip vocabulary, so it
    /// is refused rather than synthesising unbounded code (CN6, ADR-014).
    #[error("structural delta is outside the bounded round-trip vocabulary: {reason}")]
    UnboundedDelta {
        /// Why the delta is not part of the bounded set.
        reason: String,
    },

    /// A delta targeted a node id that is not present in the graph.
    #[error("structural delta targets node {id}, which is not present in the graph")]
    DeltaTargetMissing {
        /// The missing target node id.
        id: usize,
    },

    /// A regenerated identifier (an activity or module name) would not be a
    /// valid Gleam `snake_case` identifier, so emitting it would produce code
    /// that does not type-check.
    #[error("cannot regenerate Gleam: name `{name}` is not a valid Gleam identifier: {reason}")]
    RegenInvalidName {
        /// The offending name.
        name: String,
        /// Why the name cannot be used.
        reason: String,
    },
}
