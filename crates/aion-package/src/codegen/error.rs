//! Error taxonomy for the types-first codec generation.
//!
//! Every variant carries the offending module, type, field, or path as
//! structured data so callers can render actionable guidance. Subset failures
//! always name the types module and the type (and field, where one exists) of
//! the offending construct.

use std::path::PathBuf;

use crate::PackagingError;

/// Errors produced while generating codecs, schemas, and activity plumbing
/// from a workflow project's authored types module.
#[derive(thiserror::Error, Debug)]
pub enum CodegenError {
    /// The project descriptor (`workflow.toml`) or a schema it references
    /// failed to load.
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

    /// The `gleam export package-interface` JSON did not parse into the
    /// expected shape. The format is owned by the Gleam compiler; an unknown
    /// shape fails loudly rather than being guessed at.
    #[error(
        "the exported package interface did not parse: {source}; the interface JSON is \
         produced by `gleam export package-interface` (check the installed gleam version)"
    )]
    InterfaceParse {
        /// JSON parsing failure reported by `serde_json`.
        source: serde_json::Error,
    },

    /// The exported interface has no types module for the package.
    #[error(
        "the package exports no `{module}` module; declare the workflow's boundary types in \
         `src/{module}.gleam` — it is the authored source `aion generate` reads (ADR-014)"
    )]
    TypesModuleMissing {
        /// The expected types-module name (`<package>_io`).
        module: String,
    },

    /// The types module exports functions, constants, or type aliases.
    #[error(
        "`src/{module}.gleam` must declare types only, but exports: {offenders:?}; codecs are \
         generated FROM the types (run `aion generate`), never hand-written beside them"
    )]
    TypesModuleNotTypesOnly {
        /// The types-module name.
        module: String,
        /// The offending exports (`fn x`, `const y`, `type alias z`).
        offenders: Vec<String>,
    },

    /// The types module declares no public types.
    #[error("`src/{module}.gleam` declares no public types to generate codecs from")]
    TypesModuleEmpty {
        /// The types-module name.
        module: String,
    },

    /// A type in the types module falls outside the supported v1 subset.
    #[error(
        "unsupported boundary type in `{module}`: `{type_name}`{field_part}: found {found}; {hint}",
        field_part = .field.as_ref().map(|name| format!(", field `{name}`")).unwrap_or_default()
    )]
    UnsupportedType {
        /// The types-module name.
        module: String,
        /// The offending type.
        type_name: String,
        /// The offending field, when the construct is field-scoped.
        field: Option<String>,
        /// What was found.
        found: String,
        /// How to fix it.
        hint: String,
    },

    /// A hand-authored (unmarked) `*.json` file sits in the `schemas/`
    /// directory, which holds generated artifacts only.
    #[error(
        "{path} is not a generated schema artifact; schemas are emitted from the types module \
         (ADR-014) and never authored. To migrate a schema-first project: declare the boundary \
         types in src/<package>_io.gleam, delete the authored schemas/*.json, and run \
         `aion generate` — the emitted schemas replace them"
    )]
    SchemaStray {
        /// The stray file.
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

    /// A schema uses a construct outside the supported v1 subset (the `aion
    /// input` skeleton walks the emitted schema documents).
    #[error("unsupported JSON Schema construct in {file} at `{pointer}`: {construct}")]
    UnsupportedConstruct {
        /// The schema file containing the construct.
        file: PathBuf,
        /// JSON pointer to the offending construct (`` `` is the document root).
        pointer: String,
        /// What the construct was and, where helpful, what is supported.
        construct: String,
    },

    /// A generated file could not be written.
    #[error("failed to write generated file {path}: {source}")]
    Write {
        /// The path that could not be written.
        path: PathBuf,
        /// I/O failure reported while writing.
        source: std::io::Error,
    },

    /// `--check` failed: a generated file does not exist on disk.
    #[error("--check failed: generated file {path} does not exist; run `aion generate`")]
    CheckMissing {
        /// The expected generated file path.
        path: PathBuf,
    },

    /// `--check` failed: an on-disk file differs from the generated output.
    #[error(
        "--check failed: {path} differs from the generated output; \
         run `aion generate` to regenerate it"
    )]
    CheckDrift {
        /// The drifted generated file path.
        path: PathBuf,
    },

    /// An on-disk file could not be read for `--check` comparison.
    #[error("failed to read {path} for --check: {source}")]
    CheckRead {
        /// The path that could not be read.
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

    /// A declared activity references a value type not declared in the types
    /// module, so its codec cannot be generated.
    #[error(
        "activity `{activity}` {role} type `{type_name}` is not declared in \
         src/{module}.gleam (codecs are generated from the types module's public types)"
    )]
    ActivityTypeMissing {
        /// The activity whose type is unresolved.
        activity: String,
        /// Which side of the activity the type is on (`input` or `output`).
        role: &'static str,
        /// The unresolved value type name.
        type_name: String,
        /// The types module the type was expected in (`<package>_io`).
        module: String,
    },

    /// While deriving a wire-compat golden, a value type referenced a named
    /// Gleam type absent from its boundary-type definitions. The interface
    /// front-end collects every referenced definition into the closure, so
    /// this signals a generator invariant violation rather than bad author
    /// input.
    #[error(
        "internal: wire-compat golden for type `{root_type}` references undefined \
         named type `{missing}` in {file}"
    )]
    GoldenTypeUnresolved {
        /// The value type whose golden was being derived.
        root_type: String,
        /// The named type that could not be found in the definitions.
        missing: String,
        /// The emitted schema artifact whose definitions were searched.
        file: PathBuf,
    },

    /// The test-scaffold generator could not read the workflow's entry-module
    /// source, which it needs to derive the typed entry function and timer count.
    #[error("failed to read workflow entry source {path}: {source}")]
    EntrySourceRead {
        /// The entry-module source path that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The workflow's entry-module source does not yield the facts the test
    /// scaffold needs (no `aion/workflow` import, no `define` call, or an
    /// unidentifiable typed entry function).
    #[error("cannot derive test-scaffold facts from {path}: {reason}")]
    ScaffoldFacts {
        /// The entry-module source path the facts were read from.
        path: PathBuf,
        /// Why the facts could not be derived.
        reason: String,
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
    fn display_messages_name_module_type_and_field() {
        assert_eq!(
            CodegenError::UnsupportedType {
                module: "demo_io".to_owned(),
                type_name: "Order".to_owned(),
                field: Some("total".to_owned()),
                found: "a tuple type".to_owned(),
                hint: "use a record".to_owned(),
            }
            .to_string(),
            "unsupported boundary type in `demo_io`: `Order`, field `total`: \
             found a tuple type; use a record"
        );
        assert_eq!(
            CodegenError::UnsupportedType {
                module: "demo_io".to_owned(),
                type_name: "Order".to_owned(),
                field: None,
                found: "an opaque type".to_owned(),
                hint: "expose the constructors".to_owned(),
            }
            .to_string(),
            "unsupported boundary type in `demo_io`: `Order`: found an opaque type; \
             expose the constructors"
        );
        assert_eq!(
            CodegenError::CheckDrift {
                path: PathBuf::from("src/demo_codecs.gleam"),
            }
            .to_string(),
            "--check failed: src/demo_codecs.gleam differs from the generated \
             output; run `aion generate` to regenerate it"
        );
        assert_eq!(
            CodegenError::CheckMissing {
                path: PathBuf::from("src/demo_codecs.gleam"),
            }
            .to_string(),
            "--check failed: generated file src/demo_codecs.gleam does not exist; \
             run `aion generate`"
        );
        let missing = CodegenError::TypesModuleMissing {
            module: "demo_io".to_owned(),
        }
        .to_string();
        assert!(missing.contains("src/demo_io.gleam") && missing.contains("ADR-014"));
        let activity = CodegenError::ActivityTypeMissing {
            activity: "charge".to_owned(),
            role: "output",
            type_name: "Receipt".to_owned(),
            module: "demo_io".to_owned(),
        }
        .to_string();
        assert!(activity.contains("charge") && activity.contains("src/demo_io.gleam"));
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
