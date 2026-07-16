//! Local `awl` subcommand group: the rev-2 AWL authoring loop.
//!
//! `aion awl check` parses and typechecks a `.awl` document, printing one
//! compiler-style `<file>:<line>:<column>: error: <message>` diagnostic per
//! error to stderr and exiting non-zero when any is found. `aion awl fmt`
//! rewrites the document in place with the canonical printer — the printer IS
//! the formatter, one rendering — so there is deliberately no `--check` mode.
//! `aion awl emit` lowers a document to Gleam source, but only past a clean
//! typecheck — generated code quality depends on it, so a parse error, a
//! typecheck error, and an emit error all report the same way and exit
//! non-zero. `aion awl schema` derives draft 2020-12 JSON Schema from the same
//! checked document through the public `aion-awl` derivation. Schema imports
//! (`type X = schema("file")`) resolve relative to the document's directory.
//!
//! All three commands run entirely locally and own their own compiler-style
//! reporting contract (diagnostics to stderr, a one-line summary to stdout)
//! instead of the client commands' JSON rendering.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aion_awl::Span;
use clap::Subcommand;

/// The `aion awl` authoring subcommands.
#[derive(Debug, Subcommand)]
pub(crate) enum AwlCommand {
    /// Parse and typecheck a rev-2 AWL workflow document: declarations,
    /// binding flow along the step graph, `after`/route targets, outcome
    /// exhaustiveness, and both schema doors.
    ///
    /// Prints one `<file>:<line>:<column>: error: <message>` diagnostic per
    /// error to stderr and exits non-zero when any is found; prints a
    /// one-line `ok: <file> (N steps)` summary to stdout when clean.
    Check {
        /// Path to the `.awl` document.
        file: PathBuf,
    },
    /// Reformat an AWL workflow document in place.
    ///
    /// Parses the document and writes the canonical rendering back to the
    /// file — the printer is the single formatter, one rendering, so there is
    /// no `--check` mode. On a parse error the diagnostic prints to stderr,
    /// the file is left untouched, and the exit code is non-zero.
    Fmt {
        /// Path to the `.awl` document.
        file: PathBuf,
    },
    /// Generate a Gleam workflow module from an AWL document (the stopgap
    /// execution target until AWL bytecode lands).
    ///
    /// Parses and typechecks the document first — emission requires a clean
    /// typecheck, since generated code quality depends on it — then lowers
    /// the steps, forks, loops, and outcome routes onto the aion Gleam SDK.
    /// A parse error, a typecheck error, or an emit error all print
    /// `<file>:<line>:<column>: error: <message>` diagnostics to stderr and
    /// exit non-zero. On success the generated module is written to
    /// `--output` if given, otherwise to stdout.
    Emit {
        /// Path to the `.awl` document.
        file: PathBuf,
        /// Path to write the generated Gleam module. Defaults to stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Emit JSON Schema draft 2020-12 for a declared AWL type.
    Schema {
        /// Path to the `.awl` document.
        file: PathBuf,
        /// Declared type to derive; omitted, emits the workflow's start contract (its inputs).
        #[arg(long)]
        r#type: Option<String>,
    },
    /// Run the AWL language server over stdio for editor integration.
    Lsp,
}

/// Runs an `aion awl` subcommand.
pub(crate) fn run(command: &AwlCommand) -> ExitCode {
    match command {
        AwlCommand::Check { file } => check_command(file),
        AwlCommand::Fmt { file } => fmt_command(file),
        AwlCommand::Emit { file, output } => emit_command(file, output.as_deref()),
        AwlCommand::Schema { file, r#type } => schema_command(file, r#type.as_deref()),
        AwlCommand::Lsp => match aion_awl_lsp::run_stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: AWL language server failed: {error}");
                ExitCode::FAILURE
            }
        },
    }
}

fn check_command(file: &Path) -> ExitCode {
    let Some(source) = read_source(file) else {
        return ExitCode::FAILURE;
    };
    match check_source(file, &source) {
        Ok(steps) => {
            let noun = if steps == 1 { "step" } else { "steps" };
            println!("ok: {} ({steps} {noun})", file.display());
            ExitCode::SUCCESS
        }
        Err(diagnostics) => report(&diagnostics),
    }
}

fn fmt_command(file: &Path) -> ExitCode {
    let Some(source) = read_source(file) else {
        return ExitCode::FAILURE;
    };
    match format_source(file, &source) {
        Ok(formatted) => {
            if let Err(error) = fs::write(file, formatted) {
                eprintln!("error: failed to write {}: {error}", file.display());
                return ExitCode::FAILURE;
            }
            println!("formatted: {}", file.display());
            ExitCode::SUCCESS
        }
        Err(diagnostics) => report(&diagnostics),
    }
}

fn emit_command(file: &Path, output: Option<&Path>) -> ExitCode {
    let Some(source) = read_source(file) else {
        return ExitCode::FAILURE;
    };
    match emit_artifact_source(file, &source) {
        Ok(artifact) => {
            if let Some(output) = output {
                if let Err(error) = fs::write(output, &artifact.source) {
                    eprintln!("error: failed to write {}: {error}", output.display());
                    return ExitCode::FAILURE;
                }
                if let Err(error) = write_entry_sidecar(output, &artifact) {
                    eprintln!("error: failed to write generated entry metadata: {error}");
                    return ExitCode::FAILURE;
                }
                println!("emitted: {}", output.display());
            } else {
                print!("{}", artifact.source);
            }
            ExitCode::SUCCESS
        }
        Err(diagnostics) => report(&diagnostics),
    }
}

fn schema_command(file: &Path, type_name: Option<&str>) -> ExitCode {
    let Some(source) = read_source(file) else {
        return ExitCode::FAILURE;
    };
    match schema_source(file, &source, type_name) {
        Ok(schema) => {
            print!("{schema}");
            ExitCode::SUCCESS
        }
        Err(diagnostics) => report(&diagnostics),
    }
}

/// Parses and typechecks `source`, returning the workflow step count on a
/// clean pass or the rendered diagnostic lines otherwise. A parse error
/// yields the same diagnostic shape as a typecheck error.
fn check_source(file: &Path, source: &str) -> Result<usize, Vec<String>> {
    let document = aion_awl::parse(source)
        .map_err(|error| vec![diagnostic(file, error.span, &error.message)])?;
    let errors = aion_awl::check_in(&document, document_root(file));
    if errors.is_empty() {
        Ok(document.steps.len())
    } else {
        Err(errors
            .iter()
            .map(|error| diagnostic(file, error.span, &error.message))
            .collect())
    }
}

/// Parses `source` and returns the canonical rendering, or the parse
/// diagnostic. Formatting deliberately never runs the typechecker: a
/// well-formed document with type errors still deserves a canonical layout.
fn format_source(file: &Path, source: &str) -> Result<String, Vec<String>> {
    let document = aion_awl::parse(source)
        .map_err(|error| vec![diagnostic(file, error.span, &error.message)])?;
    Ok(aion_awl::print(&document))
}

/// Parses, typechecks, and emits `source` as Gleam source. Emission
/// deliberately requires a clean typecheck — unlike `format_source` — since
/// the generated code's quality depends on it: a parse error, any typecheck
/// error, or an emit error all yield the same diagnostic shape.
#[cfg(test)]
fn emit_source(file: &Path, source: &str) -> Result<String, Vec<String>> {
    Ok(emit_artifact_source(file, source)?.source)
}

fn emit_artifact_source(
    file: &Path,
    source: &str,
) -> Result<aion_awl::EmittedArtifact, Vec<String>> {
    let document = aion_awl::parse(source)
        .map_err(|error| vec![diagnostic(file, error.span, &error.message)])?;
    let root = document_root(file);
    let errors = aion_awl::check_in(&document, root);
    if !errors.is_empty() {
        return Err(errors
            .iter()
            .map(|error| diagnostic(file, error.span, &error.message))
            .collect());
    }
    aion_awl::emit_artifact_in(&document, root)
        .map_err(|error| vec![diagnostic(file, error.span, &error.message)])
}

fn write_entry_sidecar(
    output: &Path,
    artifact: &aion_awl::EmittedArtifact,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = output.with_extension("awl.json");
    if artifact.synthesized_workflows.is_empty() {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(());
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(&artifact.project_metadata())?,
    )?;
    Ok(())
}

/// The directory schema imports resolve against: the document's own.
fn document_root(file: &Path) -> &Path {
    match file.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn schema_source(
    file: &Path,
    source: &str,
    requested_type: Option<&str>,
) -> Result<String, Vec<String>> {
    let document = aion_awl::parse(source)
        .map_err(|error| vec![diagnostic(file, error.span, &error.message)])?;
    let root = document_root(file);
    let errors = aion_awl::check_in(&document, root);
    if !errors.is_empty() {
        return Err(errors
            .iter()
            .map(|error| diagnostic(file, error.span, &error.message))
            .collect());
    }
    let schema = requested_type
        .map_or_else(
            || aion_awl::schema_for_workflow_in(&document, root),
            |name| aion_awl::schema_for_type_in(&document, root, name),
        )
        .map_err(|error| vec![diagnostic(file, error.span(), &error.to_string())])?;
    serde_json::to_string_pretty(&schema)
        .map(|json| format!("{json}\n"))
        .map_err(|error| vec![diagnostic(file, document.span, &error.to_string())])
}

/// Renders one compiler-style diagnostic line from a diagnostic's span.
fn diagnostic(file: &Path, span: Span, message: &str) -> String {
    format!(
        "{}:{}:{}: error: {message}",
        file.display(),
        span.line,
        span.column
    )
}

fn report(diagnostics: &[String]) -> ExitCode {
    for line in diagnostics {
        eprintln!("{line}");
    }
    ExitCode::FAILURE
}

fn read_source(file: &Path) -> Option<String> {
    match fs::read_to_string(file) {
        Ok(source) => Some(source),
        Err(error) => {
            eprintln!("error: failed to read {}: {error}", file.display());
            None
        }
    }
}

#[cfg(test)]
#[path = "awl_tests.rs"]
mod tests;
