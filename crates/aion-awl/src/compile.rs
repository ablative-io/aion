//! The stable compile seam: one call from `.awl` source to everything
//! downstream packaging needs — deterministic `.beam` bytes, the derived
//! start/outcome contracts, action requirements, and the `.gleam_types`
//! sidecar. Wraps the `#[doc(hidden)]` MIR pipeline (lower → verify →
//! select → sidecar) without exposing any MIR type.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::ast::{Document, PipeStage, Statement, Step};
use crate::mir::{LowerError, lower, project_sidecar, select, verify};
use crate::{CheckError, ParseError, SchemaError, Span};

/// Everything the compile pipeline produces for one workflow document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledWorkflow {
    /// Workflow name declared in the AWL document.
    pub workflow_name: String,
    /// Document-declared workflow timeout; `None` leaves packaging policy to
    /// the assembler default.
    pub timeout: Option<Duration>,
    /// Deterministic `.beam` bytes (the `select` output, self-validated).
    pub beam_bytes: Vec<u8>,
    /// Derived start contract (the existing workflow-schema derivation).
    pub input_schema: Value,
    /// Derived outcome envelope (`{ "outcome": name, "payload": … }`).
    pub output_schema: Value,
    /// Effective action requirements, one row per distinct node requirement.
    pub actions: Vec<ActionRequirement>,
    /// `.gleam_types` sidecar bytes (deterministic).
    pub sidecar_bytes: Vec<u8>,
}

/// One effective action requirement: the task queue (worker name), the
/// action, and one effective node requirement — the call-site `node`
/// override where a call pins one, the action declaration's `node`
/// otherwise. An action whose call sites pin distinct nodes yields one row
/// per distinct requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRequirement {
    /// Worker (task queue) name the action is declared on.
    pub task_queue: String,
    /// Action name.
    pub action: String,
    /// Effective node requirement, `None` when unpinned.
    pub node: Option<String>,
}

/// A compile failure at any pipeline stage, carrying the existing
/// diagnostics losslessly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// The document does not parse.
    Parse(ParseError),
    /// The document does not typecheck (every diagnostic, source order).
    Check(Vec<CheckError>),
    /// A contract schema could not be derived.
    Schema(SchemaError),
    /// A shape the direct compiler does not yet lower (the BC-2 scope
    /// marker, NOT a reference refusal).
    Unsupported {
        /// The refused shape, verbatim from the lowering.
        shape: String,
        /// Source span of the refused construct.
        span: Span,
    },
    /// A span-anchored lowering failure.
    Lower {
        /// Diagnostic text, verbatim from the lowering.
        message: String,
        /// Source span of the offending construct.
        span: Span,
    },
    /// A planning refusal from the shared emitter passes (no span).
    Planning {
        /// Diagnostic text, verbatim from the planning pass.
        message: String,
    },
    /// An internal backend failure (verify/select), a bug upstream — the
    /// wrapped diagnostic rendered verbatim.
    Backend {
        /// Diagnostic text, verbatim from the backend stage.
        message: String,
    },
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "{error}"),
            // `CheckError` has no Display of its own; the structured
            // diagnostics (pinned verbatim by test) are the contract, this
            // aggregate rendering is only a fallback for `.to_string()`.
            Self::Check(errors) => {
                let mut first = true;
                for error in errors {
                    if !first {
                        writeln!(f)?;
                    }
                    first = false;
                    write!(
                        f,
                        "{} at line {}, column {}",
                        error.message, error.span.line, error.span.column
                    )?;
                }
                Ok(())
            }
            Self::Schema(error) => write!(f, "{error}"),
            // The two span-anchored lowering forms render exactly as the
            // MIR path's `LowerError` does today (pinned by test).
            Self::Unsupported { shape, span } => {
                write!(f, "BC-2 does not yet lower {shape} (line {})", span.line)
            }
            Self::Lower { message, span } => write!(f, "{message} (line {})", span.line),
            Self::Planning { message } | Self::Backend { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for CompileError {}

/// Compile `.awl` source to a [`CompiledWorkflow`], resolving schema imports
/// relative to `schema_root` (the document's directory) exactly as the CLI's
/// emit path does.
///
/// Pipeline: parse → typecheck → contract derivation → MIR lower → verify →
/// select → sidecar. Deterministic: the same source yields the same bytes.
///
/// # Errors
///
/// Returns [`CompileError`] wrapping the failing stage's existing
/// diagnostics: parse/check/schema errors verbatim, lowering refusals with
/// the unsupported/planning distinction preserved, and backend invariant
/// failures rendered verbatim.
pub fn compile(source: &str, schema_root: &Path) -> Result<CompiledWorkflow, CompileError> {
    let document = crate::parse(source).map_err(CompileError::Parse)?;
    let errors = crate::check_in(&document, schema_root);
    if !errors.is_empty() {
        return Err(CompileError::Check(errors));
    }
    let input_schema =
        crate::schema_for_workflow_in(&document, schema_root).map_err(CompileError::Schema)?;
    let output_schema =
        crate::schema_for_outcomes_in(&document, schema_root).map_err(CompileError::Schema)?;
    let actions = action_requirements(&document);
    let module = lower(&document, Some(schema_root)).map_err(lower_diagnostic)?;
    verify(&module).map_err(|error| CompileError::Backend {
        message: error.to_string(),
    })?;
    let beam_bytes = select(&module).map_err(|error| CompileError::Backend {
        message: error.to_string(),
    })?;
    let sidecar_bytes = project_sidecar(&module);
    let timeout = document.timeout.as_ref().map(workflow_timeout);
    Ok(CompiledWorkflow {
        workflow_name: document.name.clone(),
        timeout,
        beam_bytes,
        input_schema,
        output_schema,
        actions,
        sidecar_bytes,
    })
}

fn workflow_timeout(timeout: &crate::ast::WorkflowTimeoutDecl) -> Duration {
    let seconds_per_unit = match timeout.duration.unit {
        crate::DurationUnit::Seconds => 1,
        crate::DurationUnit::Minutes => 60,
        crate::DurationUnit::Hours => 60 * 60,
        crate::DurationUnit::Days => 24 * 60 * 60,
    };
    Duration::from_secs(timeout.duration.magnitude.saturating_mul(seconds_per_unit))
}

/// Derive the effective action requirements of a parsed document: for each
/// declared action, the distinct effective node requirements across its call
/// sites (a call-site `node` override wins over the action declaration's),
/// in declaration order then call-site source order. An action with no call
/// sites carries its declared requirement.
#[must_use]
pub fn action_requirements(document: &Document) -> Vec<ActionRequirement> {
    let mut requirements = Vec::new();
    for worker in &document.workers {
        for action in &worker.actions {
            let declared = action
                .config
                .as_ref()
                .and_then(|config| config.node.as_ref())
                .map(|value| value.name.as_str());
            let mut nodes = Vec::new();
            for step in &document.steps {
                step_nodes(step, &action.name, declared, &mut nodes);
            }
            if nodes.is_empty() {
                nodes.push(declared.map(str::to_owned));
            }
            for node in nodes {
                requirements.push(ActionRequirement {
                    task_queue: worker.name.clone(),
                    action: action.name.clone(),
                    node,
                });
            }
        }
    }
    requirements
}

fn step_nodes(step: &Step, action: &str, declared: Option<&str>, nodes: &mut Vec<Option<String>>) {
    statement_nodes(&step.body, action, declared, nodes);
    if let Some(on_failure) = &step.on_failure {
        statement_nodes(&on_failure.body, action, declared, nodes);
    }
}

fn statement_nodes(
    statements: &[Statement],
    action: &str,
    declared: Option<&str>,
    nodes: &mut Vec<Option<String>>,
) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                if call.call.name == action {
                    let node = call
                        .config
                        .as_ref()
                        .and_then(|config| config.node.as_ref())
                        .map_or_else(
                            || declared.map(str::to_owned),
                            |value| Some(value.name.clone()),
                        );
                    push_unique(nodes, node);
                }
            }
            Statement::Pipe(pipe) => {
                for stage in &pipe.stages {
                    if let PipeStage::Action { name, .. } = stage
                        && name == action
                    {
                        push_unique(nodes, declared.map(str::to_owned));
                    }
                }
            }
            Statement::Fork(fork) => statement_nodes(&fork.body, action, declared, nodes),
            Statement::Loop(looped) => statement_nodes(&looped.body, action, declared, nodes),
            Statement::SubStep(sub) => step_nodes(sub, action, declared, nodes),
            // `spawn` starts a child workflow, never a worker action.
            Statement::Spawn(_)
            | Statement::Wait(_)
            | Statement::Sleep(_)
            | Statement::Route(_) => {}
        }
    }
}

fn push_unique(nodes: &mut Vec<Option<String>>, node: Option<String>) {
    if !nodes.contains(&node) {
        nodes.push(node);
    }
}

/// Map a lowering failure into the public error, field-for-field.
fn lower_diagnostic(error: LowerError) -> CompileError {
    match error {
        LowerError::Unsupported { shape, span } => CompileError::Unsupported { shape, span },
        LowerError::Message { message, span } => CompileError::Lower { message, span },
        LowerError::Planning { message } => CompileError::Planning { message },
    }
}
