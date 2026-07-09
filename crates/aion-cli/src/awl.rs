//! Local `awl` subcommand group: the AWL-0 authoring loop.
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
//! checked document through the public `aion-awl` derivation.
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
    /// Parse and typecheck an AWL workflow document.
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
    /// Generate Gleam source from an AWL workflow document.
    ///
    /// Parses and typechecks the document first — emission requires a clean
    /// typecheck, since generated code quality depends on it — then lowers
    /// it to Gleam. A parse error, a typecheck error, or an emit error all
    /// print `<file>:<line>:<column>: error: <message>` diagnostics to
    /// stderr and exit non-zero. On success the generated module is written
    /// to `--output` if given, otherwise to stdout.
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
        /// Declared type to derive; defaults to the named output or sole input type.
        #[arg(long)]
        r#type: Option<String>,
    },
}

/// Runs an `aion awl` subcommand.
pub(crate) fn run(command: &AwlCommand) -> ExitCode {
    match command {
        AwlCommand::Check { file } => check_command(file),
        AwlCommand::Fmt { file } => fmt_command(file),
        AwlCommand::Emit { file, output } => emit_command(file, output.as_deref()),
        AwlCommand::Schema { file, r#type } => schema_command(file, r#type.as_deref()),
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
    match emit_source(file, &source) {
        Ok(generated) => {
            if let Some(output) = output {
                if let Err(error) = fs::write(output, generated) {
                    eprintln!("error: failed to write {}: {error}", output.display());
                    return ExitCode::FAILURE;
                }
                println!("emitted: {}", output.display());
            } else {
                print!("{generated}");
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
    let errors = aion_awl::check(&document);
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
fn emit_source(file: &Path, source: &str) -> Result<String, Vec<String>> {
    let document = aion_awl::parse(source)
        .map_err(|error| vec![diagnostic(file, error.span, &error.message)])?;
    let errors = aion_awl::check(&document);
    if !errors.is_empty() {
        return Err(errors
            .iter()
            .map(|error| diagnostic(file, error.span, &error.message))
            .collect());
    }
    aion_awl::emit(&document).map_err(|error| vec![diagnostic(file, error.span, &error.message)])
}

fn schema_source(
    file: &Path,
    source: &str,
    requested_type: Option<&str>,
) -> Result<String, Vec<String>> {
    let document = aion_awl::parse(source)
        .map_err(|error| vec![diagnostic(file, error.span, &error.message)])?;
    let errors = aion_awl::check(&document);
    if !errors.is_empty() {
        return Err(errors
            .iter()
            .map(|error| diagnostic(file, error.span, &error.message))
            .collect());
    }
    let type_name = requested_type
        .map(str::to_owned)
        .or_else(|| {
            document
                .output
                .as_ref()
                .and_then(|output| match &output.ty {
                    aion_awl::TypeRef::Named { name, .. } => Some(name.clone()),
                    _ => None,
                })
        })
        .or_else(|| {
            if document.inputs.len() == 1 {
                match &document.inputs[0].ty {
                    aion_awl::TypeRef::Named { name, .. } => Some(name.clone()),
                    _ => None,
                }
            } else {
                None
            }
        })
        .ok_or_else(|| {
            vec![diagnostic(
                file,
                document.span,
                "schema type is ambiguous; pass `--type Name`",
            )]
        })?;
    let schema = aion_awl::schema_for_type(&document, &type_name)
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
mod tests {
    use aion_awl::CheckError;

    use super::*;

    const VALID_DOC: &str = "workflow probe\n\noutput String\n\naction make() -> String\n\nstep one\n  do make()\n  as out\n\nfinish out\n";

    #[test]
    fn diagnostic_renders_the_compiler_style_line() {
        // A synthetic checker diagnostic renders as <file>:<line>:<column>.
        let error = CheckError {
            span: Span {
                start: 12,
                end: 16,
                line: 3,
                column: 7,
            },
            message: "unknown name `stat`".to_owned(),
        };
        let line = diagnostic(Path::new("flows/probe.awl"), error.span, &error.message);
        assert_eq!(line, "flows/probe.awl:3:7: error: unknown name `stat`");
    }

    #[test]
    fn check_source_counts_steps_on_a_clean_document() {
        let steps = check_source(Path::new("probe.awl"), VALID_DOC);
        assert_eq!(steps, Ok(1));
    }

    #[test]
    fn check_source_renders_a_parse_error_as_a_diagnostic() -> anyhow::Result<()> {
        let Err(diagnostics) = check_source(Path::new("probe.awl"), "not a workflow\n") else {
            anyhow::bail!("expected a parse diagnostic");
        };
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0].starts_with("probe.awl:1:1: error: "),
            "unexpected diagnostic: {}",
            diagnostics[0]
        );
        Ok(())
    }

    #[test]
    fn check_source_renders_typecheck_errors_as_diagnostics() -> anyhow::Result<()> {
        // Well-formed, but `finish` names a binding that never exists.
        let source = "workflow probe\noutput String\n\nfinish missing\n";
        let Err(diagnostics) = check_source(Path::new("probe.awl"), source) else {
            anyhow::bail!("expected a typecheck diagnostic");
        };
        assert!(!diagnostics.is_empty());
        for line in &diagnostics {
            assert!(
                line.starts_with("probe.awl:") && line.contains(": error: "),
                "unexpected diagnostic: {line}"
            );
        }
        Ok(())
    }

    #[test]
    fn format_source_is_the_canonical_printer() -> anyhow::Result<()> {
        // An already-canonical document formats to itself (one rendering).
        let formatted = format_source(Path::new("probe.awl"), VALID_DOC)
            .map_err(|d| anyhow::anyhow!("unexpected diagnostics: {d:?}"))?;
        assert_eq!(formatted, VALID_DOC);
        Ok(())
    }

    #[test]
    fn format_source_reports_a_parse_error_without_output() -> anyhow::Result<()> {
        let Err(diagnostics) = format_source(Path::new("probe.awl"), "step\n") else {
            anyhow::bail!("expected a parse diagnostic");
        };
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].starts_with("probe.awl:1:1: error: "));
        Ok(())
    }

    #[test]
    fn emit_source_generates_gleam_for_a_clean_document() -> anyhow::Result<()> {
        let generated = emit_source(Path::new("probe.awl"), VALID_DOC)
            .map_err(|d| anyhow::anyhow!("unexpected diagnostics: {d:?}"))?;
        assert!(
            generated.contains("pub fn execute"),
            "expected generated code to contain `pub fn execute`: {generated}"
        );
        Ok(())
    }

    #[test]
    fn emit_source_is_gated_on_a_clean_typecheck() -> anyhow::Result<()> {
        // Well-formed, but `finish` names a binding that never exists — emit
        // must refuse rather than generate code from an ill-typed document.
        let source = "workflow probe\noutput String\n\nfinish missing\n";
        let Err(diagnostics) = emit_source(Path::new("probe.awl"), source) else {
            anyhow::bail!("expected a typecheck diagnostic");
        };
        assert!(!diagnostics.is_empty());
        for line in &diagnostics {
            assert!(
                line.starts_with("probe.awl:") && line.contains(": error: "),
                "unexpected diagnostic: {line}"
            );
        }
        Ok(())
    }

    #[test]
    fn emit_source_renders_a_parse_error_as_a_diagnostic() -> anyhow::Result<()> {
        let Err(diagnostics) = emit_source(Path::new("probe.awl"), "not a workflow\n") else {
            anyhow::bail!("expected a parse diagnostic");
        };
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0].starts_with("probe.awl:1:1: error: "),
            "unexpected diagnostic: {}",
            diagnostics[0]
        );
        Ok(())
    }

    #[test]
    fn schema_source_matches_the_library_golden() -> anyhow::Result<()> {
        let source = include_str!("../../aion-awl/tests/fixtures/typed_contract.awl");
        let schema = schema_source(Path::new("typed_contract.awl"), source, Some("Brief"))
            .map_err(|diagnostics| anyhow::anyhow!("unexpected diagnostics: {diagnostics:?}"))?;
        assert_eq!(
            schema,
            include_str!("../../aion-awl/tests/fixtures/brief.schema.golden.json")
        );
        Ok(())
    }

    #[test]
    fn schema_source_is_gated_on_a_clean_typecheck() -> anyhow::Result<()> {
        let source =
            "workflow probe\noutput Brief\n\ntype Brief { value: Missing }\n\nfinish missing\n";
        let Err(diagnostics) = schema_source(Path::new("probe.awl"), source, Some("Brief")) else {
            anyhow::bail!("expected a typecheck diagnostic");
        };
        assert!(!diagnostics.is_empty());
        for line in &diagnostics {
            assert!(
                line.starts_with("probe.awl:") && line.contains(": error: "),
                "unexpected diagnostic: {line}"
            );
        }
        Ok(())
    }
}
