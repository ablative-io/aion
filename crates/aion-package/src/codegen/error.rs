//! Error taxonomy for Gleam codec generation.

use std::path::PathBuf;

use crate::PackagingError;

/// Errors produced while generating Gleam types and codecs from a workflow
/// project's JSON Schemas.
///
/// Every variant carries the offending file, JSON pointer, or path as
/// structured data so callers can render actionable guidance. Schema-shape
/// failures always name the schema file and the JSON pointer of the
/// offending construct.
#[derive(thiserror::Error, Debug)]
pub enum CodegenError {
    /// The project descriptor (`workflow.toml`) or a schema it references
    /// failed to load: missing descriptor, invalid TOML, a referenced schema
    /// file that does not exist, or invalid JSON in a referenced schema.
    #[error(transparent)]
    Config(#[from] PackagingError),

    /// The project's `gleam.toml` declares a package name that cannot prefix
    /// a generated Gleam module.
    #[error("gleam.toml package name `{name}` cannot name the generated module: {reason}")]
    ProjectName {
        /// The declared package name.
        name: String,
        /// Why the name cannot be used.
        reason: String,
    },

    /// A `workflow.toml` entry references a schema outside the project's
    /// `schemas/` directory, where codegen would never see it; schema/codec
    /// drift protection requires every referenced schema to live there.
    #[error(
        "invalid workflow.toml: {field}: schema {path} is outside the schemas/ directory; \
         `aion codegen` only generates from schemas/*.json"
    )]
    SchemaOutsideSchemasDir {
        /// Descriptor field that declared the path, e.g. `workflow[0].input_schema`.
        field: String,
        /// The resolved schema path.
        path: PathBuf,
    },

    /// The project has no `schemas/` directory to generate from.
    #[error("schemas directory {path} does not exist")]
    SchemasDirMissing {
        /// The expected `schemas/` directory.
        path: PathBuf,
    },

    /// The `schemas/` directory exists but contains no `*.json` files.
    #[error("no *.json schema files found in {path}")]
    SchemasDirEmpty {
        /// The searched `schemas/` directory.
        path: PathBuf,
    },

    /// The `schemas/` directory could not be listed.
    #[error("failed to list schemas directory {path}: {source}")]
    SchemasDirRead {
        /// The `schemas/` directory that could not be listed.
        path: PathBuf,
        /// I/O failure reported while listing the directory.
        source: std::io::Error,
    },

    /// A schema file could not be read.
    #[error("failed to read schema {path}: {source}")]
    SchemaRead {
        /// The schema file that could not be read.
        path: PathBuf,
        /// I/O failure reported while reading the file.
        source: std::io::Error,
    },

    /// A schema file is not valid JSON (including duplicate object keys,
    /// which would make property order ambiguous).
    #[error("schema {path} is not valid JSON: {source}")]
    SchemaParse {
        /// The schema file that failed to parse.
        path: PathBuf,
        /// JSON parsing failure reported by `serde_json`.
        source: serde_json::Error,
    },

    /// A schema file name cannot derive a Gleam type name.
    #[error("schema file name {path} cannot name a Gleam type: {reason}")]
    SchemaFileName {
        /// The offending schema file.
        path: PathBuf,
        /// Why the file name cannot derive a type name.
        reason: String,
    },

    /// A schema uses a construct outside the supported v1 subset.
    #[error("unsupported JSON Schema construct in {file} at `{pointer}`: {construct}")]
    UnsupportedConstruct {
        /// The schema file containing the construct.
        file: PathBuf,
        /// JSON pointer to the offending construct (`` `` is the document root).
        pointer: String,
        /// What the construct was and, where helpful, what is supported.
        construct: String,
    },

    /// Two schema locations derive the same generated Gleam name.
    #[error(
        "generated Gleam name `{name}` collides: derived from {first_file} at \
         `{first_pointer}` and from {second_file} at `{second_pointer}`; \
         rename one of the schema properties or files"
    )]
    NameCollision {
        /// The colliding generated type or constructor name.
        name: String,
        /// Schema file of the first derivation.
        first_file: PathBuf,
        /// JSON pointer of the first derivation.
        first_pointer: String,
        /// Schema file of the second derivation.
        second_file: PathBuf,
        /// JSON pointer of the second derivation.
        second_pointer: String,
    },

    /// The generated module could not be written.
    #[error("failed to write generated module {path}: {source}")]
    Write {
        /// The module path that could not be written.
        path: PathBuf,
        /// I/O failure reported while writing.
        source: std::io::Error,
    },

    /// `--check` failed: the generated module does not exist on disk.
    #[error("--check failed: generated module {path} does not exist; run `aion codegen`")]
    CheckMissing {
        /// The expected generated module path.
        path: PathBuf,
    },

    /// `--check` failed: the on-disk module differs from the generated output.
    #[error(
        "--check failed: {path} differs from the schema-generated output; \
         run `aion codegen` to regenerate it"
    )]
    CheckDrift {
        /// The drifted generated module path.
        path: PathBuf,
    },

    /// The on-disk module could not be read for `--check` comparison.
    #[error("failed to read {path} for --check: {source}")]
    CheckRead {
        /// The module path that could not be read.
        path: PathBuf,
        /// I/O failure reported while reading.
        source: std::io::Error,
    },

    /// The activity manifest JSON emitted by the package's `manifest()` export
    /// is not a valid declaration array.
    #[error("activity manifest is not valid declaration JSON: {source}")]
    ManifestParse {
        /// JSON parsing failure reported by `serde_json`.
        source: serde_json::Error,
    },

    /// A declared activity name cannot name an engine activity or a generated
    /// artifact (empty, or carrying a path separator, backslash, or the
    /// deployed-name separator).
    #[error("activity name `{name}` is invalid: {reason}")]
    InvalidActivityName {
        /// The offending activity name.
        name: String,
        /// Why the name cannot be used.
        reason: String,
    },

    /// Two declarations share an activity name; names must be unique within a
    /// package so the generated wrappers, registration entries, and
    /// `workflow.toml` list are unambiguous.
    #[error("activity `{name}` is declared more than once")]
    DuplicateActivity {
        /// The duplicated activity name.
        name: String,
    },

    /// A declaration carries a tier outside the supported set.
    #[error("unknown activity tier `{value}`; expected `in_vm`, `remote_python`, or `remote_rust`")]
    UnknownTier {
        /// The unrecognised tier string.
        value: String,
    },

    /// A declared activity references a value type with no `schemas/<type>.json`
    /// document, so its codec cannot be generated.
    #[error(
        "activity `{activity}` {role} type `{type_name}` has no schema: expected {path} \
         (codecs are generated from schemas/*.json)"
    )]
    ActivitySchemaMissing {
        /// The activity whose type is unresolved.
        activity: String,
        /// Which side of the activity the type is on (`input` or `output`).
        role: &'static str,
        /// The unresolved value type name.
        type_name: String,
        /// The schema path that was expected to exist.
        path: PathBuf,
    },

    /// While deriving a wire-compat golden, a value type referenced a named
    /// Gleam type absent from its schema artifact's definitions. The schema
    /// walker emits every nested record and enum into the artifact, so this
    /// signals a generator invariant violation rather than bad author input.
    #[error(
        "internal: wire-compat golden for type `{root_type}` references undefined \
         named type `{missing}` in {file}"
    )]
    GoldenTypeUnresolved {
        /// The value type whose golden was being derived.
        root_type: String,
        /// The named type that could not be found in the artifact definitions.
        missing: String,
        /// The schema artifact whose definitions were searched.
        file: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::CodegenError;

    fn assert_send_sync<T: Send + Sync + 'static>() {}

    #[test]
    fn codegen_error_is_send_sync_and_static() {
        assert_send_sync::<CodegenError>();
    }

    #[test]
    fn display_messages_name_file_and_pointer() {
        assert_eq!(
            CodegenError::UnsupportedConstruct {
                file: PathBuf::from("schemas/input.json"),
                pointer: "/properties/tag/oneOf".to_owned(),
                construct: "unrecognised keyword `oneOf`".to_owned(),
            }
            .to_string(),
            "unsupported JSON Schema construct in schemas/input.json at \
             `/properties/tag/oneOf`: unrecognised keyword `oneOf`"
        );
        assert_eq!(
            CodegenError::CheckDrift {
                path: PathBuf::from("src/demo_io.gleam"),
            }
            .to_string(),
            "--check failed: src/demo_io.gleam differs from the schema-generated \
             output; run `aion codegen` to regenerate it"
        );
        assert_eq!(
            CodegenError::CheckMissing {
                path: PathBuf::from("src/demo_io.gleam"),
            }
            .to_string(),
            "--check failed: generated module src/demo_io.gleam does not exist; \
             run `aion codegen`"
        );
        assert_eq!(
            CodegenError::SchemaOutsideSchemasDir {
                field: "workflow[0].input_schema".to_owned(),
                path: PathBuf::from("/project/io/input.json"),
            }
            .to_string(),
            "invalid workflow.toml: workflow[0].input_schema: schema \
             /project/io/input.json is outside the schemas/ directory; \
             `aion codegen` only generates from schemas/*.json"
        );
    }

    #[test]
    fn packaging_errors_convert_transparently() {
        let error = CodegenError::from(crate::PackagingError::ConfigMissing {
            root: PathBuf::from("/project"),
        });

        assert_eq!(error.to_string(), "no workflow.toml found in /project");
        assert!(matches!(error, CodegenError::Config(_)));
    }
}
