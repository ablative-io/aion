//! Programmatic `.aion` assembly for direct-compiled AWL workflows.
//!
//! This is the native-path counterpart of the legacy
//! [`aion_package::package_project`] pipeline: it takes a
//! [`CompiledWorkflow`] (the aion-awl compile seam's output) and produces
//! complete format-v1 archive bytes with a fully derived manifest and the
//! embedded SDK BEAM closure — no `workflow.toml`, no built Gleam tree, no
//! toolchain invocation anywhere on this path.

use std::time::Duration;

use aion_awl::CompiledWorkflow;

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    PackageBuilder, PackageError, WorkflowEntry, builder::is_safe_logical_name,
};

use crate::bundle;

/// Manifest workflow timeout applied to AWL-native packages.
///
/// This explicit policy default applies when an AWL document does not declare
/// its own workflow timeout. It is never inferred from action timeouts.
pub const DEFAULT_WORKFLOW_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Options for [`assemble_awl`].
///
/// Construct via [`Default`] and assign fields, so call sites keep compiling
/// when options are added.
#[derive(Clone, Copy, Debug, Default)]
pub struct AwlAssembleOptions {
    /// Overrides the manifest workflow timeout; `None` applies
    /// [`DEFAULT_WORKFLOW_TIMEOUT`].
    pub timeout: Option<Duration>,
}

/// A typed refusal from [`assemble_awl`].
#[derive(Debug, thiserror::Error)]
pub enum AssembleError {
    /// The workflow name cannot serve as an archive logical module name.
    #[error("workflow name `{name}` is not a valid logical module name")]
    InvalidWorkflowName {
        /// The refused workflow name.
        name: String,
    },
    /// The workflow name collides with a module shipped in the embedded SDK
    /// closure, which would make the archive's module set ambiguous.
    #[error("workflow name `{module}` collides with an SDK closure module")]
    BundleCollision {
        /// The colliding module name.
        module: String,
    },
    /// A synthesized child workflow type cannot serve as a routing name.
    #[error("synthesized workflow type `{name}` is not a valid logical name")]
    InvalidSynthesizedName {
        /// The refused synthesized workflow type.
        name: String,
    },
    /// Two entries in one archive would claim the same workflow type.
    #[error("workflow type `{name}` appears more than once in the archive")]
    DuplicateWorkflowType {
        /// The duplicated workflow type.
        name: String,
    },
    /// The underlying package machinery refused the assembly.
    #[error(transparent)]
    Package(#[from] PackageError),
}

/// Assembles a complete format-v1 `.aion` archive from a compiled AWL
/// workflow: the workflow BEAM plus the embedded SDK closure, under a fully
/// derived manifest (entry module = workflow name, entry function = `run` —
/// the direct compiler's fixed ABI — schemas and activity names from the
/// compile output).
///
/// The archive round-trips through [`aion_package::Package::load_from_bytes`]
/// unchanged; deploy, storage, and runtime need no awareness of how it was
/// produced. Deterministic: the same compile output yields byte-identical
/// archives.
///
/// The `.awl` source text and `.gleam_types` sidecar are deliberately NOT
/// archived in this increment: the format-v1 loader rejects unknown entries,
/// so source provenance is deferred to a loader-tolerance follow-up rather
/// than shipped in a form the loader would refuse.
///
/// # Errors
///
/// Returns [`AssembleError`] for workflow names that cannot name an archive
/// module or that collide with the SDK closure, and wrapped
/// [`PackageError`]s from the shared builder machinery.
pub fn assemble_awl(
    compiled: &CompiledWorkflow,
    opts: AwlAssembleOptions,
) -> Result<Vec<u8>, AssembleError> {
    // AWL workflow names are lexer-enforced snake_case identifiers, so the
    // name doubles as the BEAM module name the direct compiler emits; the
    // guards below make the contract explicit rather than assumed.
    let name = compiled.workflow_name.as_str();
    if !is_safe_logical_name(name) {
        return Err(AssembleError::InvalidWorkflowName {
            name: name.to_owned(),
        });
    }
    if bundle::contains(name) {
        return Err(AssembleError::BundleCollision {
            module: name.to_owned(),
        });
    }

    let mut modules: Vec<BeamModule> = bundle::sdk_closure_modules()
        .map(|(module, bytes)| BeamModule::new(module, bytes))
        .collect();
    modules.push(BeamModule::new(name, compiled.beam_bytes.clone()));
    let beams = BeamSet::new(modules)?;

    let manifest = Manifest {
        entry_module: name.to_owned(),
        entry_function: "run".to_owned(),
        input_schema: compiled.input_schema.clone(),
        output_schema: compiled.output_schema.clone(),
        timeout: opts.timeout.unwrap_or(DEFAULT_WORKFLOW_TIMEOUT),
        activities: declared_activities(compiled),
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: additional_workflows(compiled)?,
    };

    let mut builder = PackageBuilder::new(manifest, beams);
    if opts.timeout.is_some() {
        builder = builder.with_explicit_timeout_identity();
    }
    Ok(builder.write_to_bytes()?)
}

/// Maps the compile output's synthesized workflow entries (implicit per-item
/// children of parallel multi-step `distribute` regions) onto the manifest's
/// `additional_workflows`, validating routing names and archive-wide
/// workflow-type uniqueness (the loader re-checks both; refusing here gives a
/// typed error at assembly time).
fn additional_workflows(compiled: &CompiledWorkflow) -> Result<Vec<WorkflowEntry>, AssembleError> {
    let mut seen: Vec<&str> = vec![compiled.workflow_name.as_str()];
    let mut entries = Vec::with_capacity(compiled.synthesized_workflows.len());
    for entry in &compiled.synthesized_workflows {
        if !is_safe_logical_name(&entry.workflow_type) {
            return Err(AssembleError::InvalidSynthesizedName {
                name: entry.workflow_type.clone(),
            });
        }
        if seen.contains(&entry.workflow_type.as_str()) {
            return Err(AssembleError::DuplicateWorkflowType {
                name: entry.workflow_type.clone(),
            });
        }
        seen.push(entry.workflow_type.as_str());
        entries.push(WorkflowEntry {
            workflow_type: entry.workflow_type.clone(),
            entry_module: entry.entry_module.clone(),
            entry_function: entry.entry_function.clone(),
            input_schema: entry.input_schema.clone(),
            output_schema: entry.output_schema.clone(),
            timeout: Duration::from_secs(entry.timeout_seconds),
            internal: entry.internal,
        });
    }
    Ok(entries)
}

/// Projects the compile output's action requirements onto the manifest's
/// activity list: one entry per distinct action name, first-appearance order
/// (requirements carry one row per distinct node pin, so names can repeat).
fn declared_activities(compiled: &CompiledWorkflow) -> Vec<DeclaredActivity> {
    let mut names: Vec<&str> = Vec::new();
    for action in &compiled.actions {
        if !names.contains(&action.action.as_str()) {
            names.push(&action.action);
        }
    }
    names
        .into_iter()
        .map(|activity_type| DeclaredActivity {
            activity_type: activity_type.to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use aion_awl::{ActionRequirement, CompiledWorkflow};
    use serde_json::json;

    use super::{AwlAssembleOptions, DEFAULT_WORKFLOW_TIMEOUT, assemble_awl};
    use aion_package::{ExtractionLimits, Package};

    fn hand_built(name: &str) -> CompiledWorkflow {
        CompiledWorkflow {
            workflow_name: name.to_owned(),
            first_worker: Some("q".to_owned()),
            timeout: None,
            beam_bytes: b"opaque-beam-bytes".to_vec(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            actions: vec![
                ActionRequirement {
                    task_queue: "q".to_owned(),
                    action: "greet".to_owned(),
                    node: None,
                },
                ActionRequirement {
                    task_queue: "q".to_owned(),
                    action: "greet".to_owned(),
                    node: Some("node-a".to_owned()),
                },
                ActionRequirement {
                    task_queue: "q".to_owned(),
                    action: "shout".to_owned(),
                    node: None,
                },
            ],
            sidecar_bytes: Vec::new(),
            synthesized_workflows: Vec::new(),
        }
    }

    /// Per-node requirement rows for one action collapse to a single
    /// manifest activity entry, in first-appearance order.
    #[test]
    fn repeated_action_requirements_dedupe_into_one_activity()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = assemble_awl(&hand_built("dedupe_case"), AwlAssembleOptions::default())?;
        let package = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

        let activities: Vec<&str> = package
            .manifest()
            .activities
            .iter()
            .map(|activity| activity.activity_type.as_str())
            .collect();
        assert_eq!(activities, vec!["greet", "shout"]);
        Ok(())
    }

    #[test]
    fn timeout_defaults_to_the_policy_constant_and_override_wins()
    -> Result<(), Box<dyn std::error::Error>> {
        let defaulted = assemble_awl(&hand_built("timeout_case"), AwlAssembleOptions::default())?;
        let package = Package::load_from_bytes(defaulted, ExtractionLimits::unbounded())?;
        assert_eq!(package.manifest().timeout, DEFAULT_WORKFLOW_TIMEOUT);

        let overridden = assemble_awl(
            &hand_built("timeout_case"),
            AwlAssembleOptions {
                timeout: Some(std::time::Duration::from_secs(90)),
            },
        )?;
        let package = Package::load_from_bytes(overridden, ExtractionLimits::unbounded())?;
        assert_eq!(
            package.manifest().timeout,
            std::time::Duration::from_secs(90)
        );
        Ok(())
    }
}
