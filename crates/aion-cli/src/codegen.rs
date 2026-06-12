//! Local `codegen` subcommand: a thin shell over
//! [`aion_package::codegen_project`].

use std::path::Path;

use aion_package::{CodegenMode, codegen_project};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::output::to_value;

/// JSON document printed on stdout after a successful `codegen` run.
#[derive(Serialize)]
struct CodegenOutput {
    /// Generated module path relative to the project root.
    module: String,
    /// Schema files generated from, in generation order.
    schemas: Vec<String>,
    /// `written` for a generation run, `checked` for `--check`.
    action: &'static str,
}

/// Runs the `codegen` subcommand: generates `src/<package>_io.gleam` from
/// the project's `schemas/*.json`, or verifies it with `--check`.
pub(crate) fn run(path: &Path, check: bool) -> Result<Value> {
    let mode = if check {
        CodegenMode::Check
    } else {
        CodegenMode::Write
    };
    let report = codegen_project(path, mode).with_context(|| {
        format!(
            "failed to generate Gleam codecs for workflow project at {}",
            path.display()
        )
    })?;
    to_value(CodegenOutput {
        module: report.module_relative,
        schemas: report.schemas,
        action: if check { "checked" } else { "written" },
    })
}
