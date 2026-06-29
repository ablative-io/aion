//! Embedded scaffold templates for `aion new`.
//!
//! Each template is a hand-rolled manifest of `(target path, contents)`
//! pairs embedded with `include_str!` — no templating engine, no extra
//! dependency. Target paths and contents may carry the `{{name}}`
//! placeholder; the worker manifest additionally carries
//! `{{aion_worker_version}}`. Substitution lives in
//! [`crate::new::scaffold`].

use clap::ValueEnum;

use crate::new::agent::{AGENT_ACTIVITIES, AGENT_FILES, AGENT_WORKER_FILES};

/// One scaffolded file: the project-relative target path and the embedded
/// contents, both before placeholder substitution.
pub type ManifestFile = (&'static str, &'static str);

/// Scaffold templates selectable with `aion new --template`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Template {
    /// Minimal typed workflow with no activities: start → complete.
    HelloWorld,
    /// Approval signal raced against a durable timeout, with a stage query
    /// and every outcome path.
    ApprovalFlow,
    /// Worker-served payment activity with workflow-driven retries, an
    /// approval race, a status query, and refund compensation.
    Saga,
    /// The durable dev pipeline: an agent develops a brief, a scoped
    /// verify-fix loop, a workspace gate, human review by signal, land —
    /// three composed workflows plus a CLI-shelling worker. Workflow-level
    /// I/O codecs are generated from the schemas by `aion codegen`.
    /// Requires `--worker rust`.
    DevPipeline,
    /// A durable agent loop: scout → act → verify → signal-gated human
    /// review, parameterised by prompts and a review deadline. The three
    /// agent steps are worker-served activities; the human approval pause is
    /// a durable `workflow.receive` with a timeout. Bundles no agent runtime
    /// — the driver stays worker-side. Requires `--worker rust`.
    Agent,
}

/// Files every template emits. The dev-pipeline template replaces the
/// shared `gleam.toml` with its own (it needs a `gleeunit` dev-dependency
/// for the scaffolded hermetic test suite).
const SHARED_FILES: &[ManifestFile] = &[
    (
        "gleam.toml",
        include_str!("../../templates/shared/gleam.toml"),
    ),
    (
        ".gitignore",
        include_str!("../../templates/shared/gitignore"),
    ),
    (
        "aion.toml",
        include_str!("../../templates/shared/aion.toml"),
    ),
];

/// Shared files minus `gleam.toml`, for templates carrying their own.
const SHARED_FILES_WITHOUT_GLEAM_TOML: &[ManifestFile] = &[
    (
        ".gitignore",
        include_str!("../../templates/shared/gitignore"),
    ),
    (
        "aion.toml",
        include_str!("../../templates/shared/aion.toml"),
    ),
];

const HELLO_WORLD_FILES: &[ManifestFile] = &[
    (
        "workflow.toml",
        include_str!("../../templates/hello_world/workflow.toml"),
    ),
    (
        "schemas/input.json",
        include_str!("../../templates/hello_world/schemas/input.json"),
    ),
    (
        "schemas/output.json",
        include_str!("../../templates/hello_world/schemas/output.json"),
    ),
    (
        "src/{{name}}.gleam",
        include_str!("../../templates/hello_world/project.gleam"),
    ),
    (
        "README.md",
        include_str!("../../templates/hello_world/README.md"),
    ),
];

const APPROVAL_FLOW_FILES: &[ManifestFile] = &[
    (
        "workflow.toml",
        include_str!("../../templates/approval_flow/workflow.toml"),
    ),
    (
        "schemas/input.json",
        include_str!("../../templates/approval_flow/schemas/input.json"),
    ),
    (
        "schemas/output.json",
        include_str!("../../templates/approval_flow/schemas/output.json"),
    ),
    (
        "src/{{name}}.gleam",
        include_str!("../../templates/approval_flow/project.gleam"),
    ),
    (
        "README.md",
        include_str!("../../templates/approval_flow/README.md"),
    ),
];

const SAGA_FILES: &[ManifestFile] = &[
    (
        "workflow.toml",
        include_str!("../../templates/saga/workflow.toml"),
    ),
    (
        "schemas/input.json",
        include_str!("../../templates/saga/schemas/input.json"),
    ),
    (
        "schemas/output.json",
        include_str!("../../templates/saga/schemas/output.json"),
    ),
    (
        "src/{{name}}.gleam",
        include_str!("../../templates/saga/project.gleam"),
    ),
    ("README.md", include_str!("../../templates/saga/README.md")),
];

/// Rust worker crate serving the saga template's activities.
///
/// The on-disk template is `Cargo.toml.tmpl`, not `Cargo.toml`: cargo
/// excludes subdirectories containing a `Cargo.toml` when packaging this
/// crate, which would break `include_str!` on the published crate.
const SAGA_WORKER_FILES: &[ManifestFile] = &[
    (
        "worker/Cargo.toml",
        include_str!("../../templates/saga/worker/Cargo.toml.tmpl"),
    ),
    (
        "worker/src/main.rs",
        include_str!("../../templates/saga/worker/main.rs"),
    ),
];

const DEV_PIPELINE_FILES: &[ManifestFile] = &[
    (
        "gleam.toml",
        include_str!("../../templates/dev_pipeline/gleam.toml"),
    ),
    (
        "workflow.toml",
        include_str!("../../templates/dev_pipeline/workflow.toml"),
    ),
    (
        "schemas/input.json",
        include_str!("../../templates/dev_pipeline/schemas/input.json"),
    ),
    (
        "schemas/output.json",
        include_str!("../../templates/dev_pipeline/schemas/output.json"),
    ),
    (
        "schemas/dev_input.json",
        include_str!("../../templates/dev_pipeline/schemas/dev_input.json"),
    ),
    (
        "schemas/dev_output.json",
        include_str!("../../templates/dev_pipeline/schemas/dev_output.json"),
    ),
    (
        "schemas/gate_input.json",
        include_str!("../../templates/dev_pipeline/schemas/gate_input.json"),
    ),
    (
        "schemas/gate_output.json",
        include_str!("../../templates/dev_pipeline/schemas/gate_output.json"),
    ),
    (
        "src/{{name}}.gleam",
        include_str!("../../templates/dev_pipeline/project.gleam"),
    ),
    (
        "src/{{name}}_dev.gleam",
        include_str!("../../templates/dev_pipeline/project_dev.gleam"),
    ),
    (
        "src/{{name}}_gate.gleam",
        include_str!("../../templates/dev_pipeline/project_gate.gleam"),
    ),
    (
        "src/{{name}}_cli_ffi.erl",
        include_str!("../../templates/dev_pipeline/cli_ffi.erl"),
    ),
    (
        "src/{{name}}/types.gleam",
        include_str!("../../templates/dev_pipeline/support/types.gleam"),
    ),
    (
        "src/{{name}}/codecs_core.gleam",
        include_str!("../../templates/dev_pipeline/support/codecs_core.gleam"),
    ),
    (
        "src/{{name}}/codecs_flow.gleam",
        include_str!("../../templates/dev_pipeline/support/codecs_flow.gleam"),
    ),
    (
        "src/{{name}}/codecs_workflows.gleam",
        include_str!("../../templates/dev_pipeline/support/codecs_workflows.gleam"),
    ),
    (
        "src/{{name}}/io_convert.gleam",
        include_str!("../../templates/dev_pipeline/support/io_convert.gleam"),
    ),
    (
        "src/{{name}}/activities.gleam",
        include_str!("../../templates/dev_pipeline/support/activities.gleam"),
    ),
    (
        "src/{{name}}/locals.gleam",
        include_str!("../../templates/dev_pipeline/support/locals.gleam"),
    ),
    (
        "src/{{name}}/cli.gleam",
        include_str!("../../templates/dev_pipeline/support/cli.gleam"),
    ),
    (
        "src/{{name}}/errors.gleam",
        include_str!("../../templates/dev_pipeline/support/errors.gleam"),
    ),
    (
        "test/{{name}}_test.gleam",
        include_str!("../../templates/dev_pipeline/test/project_test.gleam"),
    ),
    (
        "test/{{name}}_test_ffi.erl",
        include_str!("../../templates/dev_pipeline/test/test_ffi.erl"),
    ),
    (
        "test/support/shims.gleam",
        include_str!("../../templates/dev_pipeline/test/shims.gleam"),
    ),
    (
        "README.md",
        include_str!("../../templates/dev_pipeline/README.md"),
    ),
];

/// Rust worker crate serving the dev-pipeline template's eight activities
/// (required: the pipeline is meaningless without a worker serving them).
const DEV_PIPELINE_WORKER_FILES: &[ManifestFile] = &[
    (
        "worker/Cargo.toml",
        include_str!("../../templates/dev_pipeline/worker/Cargo.toml.tmpl"),
    ),
    (
        "worker/src/main.rs",
        include_str!("../../templates/dev_pipeline/worker/main.rs"),
    ),
    (
        "worker/src/lib.rs",
        include_str!("../../templates/dev_pipeline/worker/lib.rs"),
    ),
    (
        "worker/src/types.rs",
        include_str!("../../templates/dev_pipeline/worker/types.rs"),
    ),
    (
        "worker/src/handlers.rs",
        include_str!("../../templates/dev_pipeline/worker/handlers.rs"),
    ),
    (
        "worker/src/shell.rs",
        include_str!("../../templates/dev_pipeline/worker/shell.rs"),
    ),
    (
        "worker/tests/wire_compat.rs",
        include_str!("../../templates/dev_pipeline/worker/tests/wire_compat.rs"),
    ),
    (
        "worker/tests/handlers_shims.rs",
        include_str!("../../templates/dev_pipeline/worker/tests/handlers_shims.rs"),
    ),
];

impl Template {
    /// The kebab-case template name used by `--template`, `--help`, and the
    /// JSON output.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::HelloWorld => "hello-world",
            Self::ApprovalFlow => "approval-flow",
            Self::Saga => "saga",
            Self::DevPipeline => "dev-pipeline",
            Self::Agent => "agent",
        }
    }

    /// Every project file this template emits, shared files first.
    #[must_use]
    pub fn files(self) -> Vec<ManifestFile> {
        let (shared, own): (&[ManifestFile], &[ManifestFile]) = match self {
            Self::HelloWorld => (SHARED_FILES, HELLO_WORLD_FILES),
            Self::ApprovalFlow => (SHARED_FILES, APPROVAL_FLOW_FILES),
            Self::Saga => (SHARED_FILES, SAGA_FILES),
            // The dev pipeline carries its own gleam.toml (gleeunit
            // dev-dependency for the scaffolded test suite).
            Self::DevPipeline => (SHARED_FILES_WITHOUT_GLEAM_TOML, DEV_PIPELINE_FILES),
            Self::Agent => (SHARED_FILES, AGENT_FILES),
        };
        let mut files = shared.to_vec();
        files.extend_from_slice(own);
        files
    }

    /// The activities this template's workflows dispatch to a worker.
    #[must_use]
    pub fn activities(self) -> &'static [&'static str] {
        match self {
            Self::HelloWorld | Self::ApprovalFlow => &[],
            Self::Saga => &["charge_payment", "refund_payment"],
            Self::Agent => AGENT_ACTIVITIES,
            Self::DevPipeline => &[
                "provision_workspace",
                "warm_build",
                "dev",
                "scoped_checks",
                "dev_resume",
                "full_checks",
                "request_review",
                "land",
            ],
        }
    }

    /// Additional files emitted with `--worker rust`. Empty for templates
    /// whose workflows dispatch no activities — there is nothing for a
    /// worker to serve, and `aion new` refuses rather than inventing one.
    #[must_use]
    pub fn worker_files(self) -> &'static [ManifestFile] {
        match self {
            Self::HelloWorld | Self::ApprovalFlow => &[],
            Self::Saga => SAGA_WORKER_FILES,
            Self::Agent => AGENT_WORKER_FILES,
            Self::DevPipeline => DEV_PIPELINE_WORKER_FILES,
        }
    }

    /// The reason a worker-required template cannot be scaffolded without one,
    /// surfaced verbatim in the `aion new` refusal, and the single source of
    /// truth for whether `--worker` is mandatory (`Some` ⟺ required). A
    /// worker-required template's activities are all worker-served in a live
    /// deployment, so scaffolding it without the worker would emit a project
    /// that cannot run; `aion new` refuses instead. Returns `None` for
    /// templates that do not require a worker.
    #[must_use]
    pub fn worker_requirement_reason(self) -> Option<&'static str> {
        match self {
            Self::HelloWorld | Self::ApprovalFlow | Self::Saga => None,
            Self::DevPipeline => Some(
                "all eight of its activities (provision, warm build, dev agent, checks, gate, \
                 review request, land) are served by the standalone worker crate in a live \
                 deployment",
            ),
            Self::Agent => Some(
                "its three agent steps (scout, act, verify) are served by the worker, where the \
                 agent driver lives — the scaffold bundles no runtime of its own",
            ),
        }
    }

    /// Whether the scaffold runs `aion codegen` after writing files, to
    /// generate `src/<name>_io.gleam` from the emitted schemas. The
    /// template's sources import that module, so scaffolding without it
    /// would not compile.
    #[must_use]
    pub fn generates_codecs(self) -> bool {
        match self {
            Self::HelloWorld | Self::ApprovalFlow | Self::Saga | Self::Agent => false,
            Self::DevPipeline => true,
        }
    }

    /// All templates, for manifest-completeness tests.
    #[cfg(test)]
    pub fn all() -> &'static [Self] {
        &[
            Self::HelloWorld,
            Self::ApprovalFlow,
            Self::Saga,
            Self::DevPipeline,
            Self::Agent,
        ]
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::Template;

    /// Files every scaffolded project must contain, placeholder form.
    const REQUIRED_PATHS: &[&str] = &[
        "gleam.toml",
        ".gitignore",
        "aion.toml",
        "workflow.toml",
        "schemas/input.json",
        "schemas/output.json",
        "src/{{name}}.gleam",
        "README.md",
    ];

    #[test]
    fn every_template_manifest_is_complete() {
        for template in Template::all() {
            let files = template.files();
            let paths: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
            for required in REQUIRED_PATHS {
                assert!(
                    paths.contains(required),
                    "template {} is missing {required}",
                    template.id()
                );
            }
            let unique: HashSet<&str> = paths.iter().copied().collect();
            assert_eq!(
                unique.len(),
                paths.len(),
                "template {} declares duplicate paths",
                template.id()
            );
            for (path, contents) in &files {
                assert!(
                    !contents.trim().is_empty(),
                    "template {} embeds empty contents for {path}",
                    template.id()
                );
            }
        }
    }

    #[test]
    fn workflow_descriptors_declare_the_template_activities() {
        for template in Template::all() {
            let files = template.files();
            let workflow_toml = files
                .iter()
                .find(|(path, _)| *path == "workflow.toml")
                .map(|(_, contents)| *contents)
                .unwrap_or_default();
            for activity in template.activities() {
                assert!(
                    workflow_toml.contains(&format!("\"{activity}\"")),
                    "template {} workflow.toml does not declare activity {activity}",
                    template.id()
                );
            }
            assert!(
                workflow_toml.contains("entry_module = \"{{name}}\""),
                "template {} workflow.toml must name the project module as entry",
                template.id()
            );
        }
    }

    #[test]
    fn aion_toml_carries_every_required_server_key() {
        for template in Template::all() {
            let files = template.files();
            let aion_toml = files
                .iter()
                .find(|(path, _)| *path == "aion.toml")
                .map(|(_, contents)| *contents)
                .unwrap_or_default();
            for key in [
                "listen_address",
                "grpc_address",
                // The scaffold ships the durable ablative default backend.
                "backend = \"haematite\"",
                "data_dir = \"aion-data\"",
                "query_timeout_ms",
                "event_broadcast_capacity",
                // Required since WS3: the cluster topology broadcast capacity, so
                // the scaffolded config boots without further edits.
                "cluster_broadcast_capacity",
                "enabled = true",
                "max_archive_bytes",
                "max_inflated_bytes",
            ] {
                assert!(
                    aion_toml.contains(key),
                    "template {} aion.toml is missing {key}",
                    template.id()
                );
            }
        }
    }

    #[test]
    fn dev_pipeline_declares_three_workflow_entries_and_its_gates() {
        let files = Template::DevPipeline.files();
        let workflow_toml = files
            .iter()
            .find(|(path, _)| *path == "workflow.toml")
            .map(|(_, contents)| *contents)
            .unwrap_or_default();
        for entry in [
            "entry_module = \"{{name}}\"",
            "entry_module = \"{{name}}_dev\"",
            "entry_module = \"{{name}}_gate\"",
        ] {
            assert!(
                workflow_toml.contains(entry),
                "dev-pipeline workflow.toml must declare {entry}"
            );
        }
        for timeout in [
            "timeout_seconds = 604800",
            "timeout_seconds = 86400",
            "timeout_seconds = 21600",
        ] {
            assert!(
                workflow_toml.contains(timeout),
                "dev-pipeline workflow.toml must keep the documented {timeout}"
            );
        }
        assert!(Template::DevPipeline.worker_requirement_reason().is_some());
        assert!(Template::DevPipeline.generates_codecs());
    }

    #[test]
    fn worker_free_templates_neither_require_a_worker_nor_run_codegen() {
        for template in [Template::HelloWorld, Template::ApprovalFlow, Template::Saga] {
            assert!(
                template.worker_requirement_reason().is_none(),
                "template {} must not require a worker",
                template.id()
            );
            assert!(
                !template.generates_codecs(),
                "template {} must not run codegen",
                template.id()
            );
        }
    }

    #[test]
    fn only_the_dev_pipeline_runs_codegen() {
        for template in Template::all() {
            assert_eq!(
                template.generates_codecs(),
                *template == Template::DevPipeline,
                "only the dev-pipeline template runs codegen; {} disagrees",
                template.id()
            );
        }
    }

    #[test]
    fn agent_template_requires_a_worker_without_codegen() {
        // The agent loop's three steps are worker-served and the driver is
        // worker-side, so the template demands `--worker rust`; its codecs are
        // hand-written in the workflow source, so it does not run codegen.
        assert!(Template::Agent.worker_requirement_reason().is_some());
        assert!(!Template::Agent.generates_codecs());
    }

    #[test]
    fn agent_template_declares_its_three_steps_and_a_review_deadline_input() {
        let files = Template::Agent.files();
        let workflow_toml = files
            .iter()
            .find(|(path, _)| *path == "workflow.toml")
            .map(|(_, contents)| *contents)
            .unwrap_or_default();
        for activity in ["scout", "act", "verify"] {
            assert!(
                workflow_toml.contains(&format!("\"{activity}\"")),
                "agent workflow.toml must declare the {activity} step"
            );
        }

        // The human-review pause is a durable `workflow.receive` raced against
        // a caller-chosen deadline — never a poll, never a default.
        let project = files
            .iter()
            .find(|(path, _)| *path == "src/{{name}}.gleam")
            .map(|(_, contents)| *contents)
            .unwrap_or_default();
        let condensed: String = project.split_whitespace().collect();
        assert!(
            condensed.contains("workflow.with_timeout(fn(){workflow.receive("),
            "agent review must be workflow.receive raced by with_timeout, not a poll"
        );
        assert!(
            condensed.contains("duration.milliseconds(input.review_timeout_ms)"),
            "agent review deadline must come from the start input, never a default"
        );

        // The review deadline is a required start-input field, not defaulted.
        let input_schema = files
            .iter()
            .find(|(path, _)| *path == "schemas/input.json")
            .map(|(_, contents)| *contents)
            .unwrap_or_default();
        assert!(
            input_schema.contains("\"review_timeout_ms\""),
            "agent input schema must require the review deadline"
        );
    }

    #[test]
    fn dev_pipeline_schemas_avoid_codegen_rejected_constructs() {
        // `aion codegen` v1 loudly rejects $ref/$defs indirection; the
        // template's schemas must stay inside the supported subset or the
        // scaffold itself would fail.
        for (path, contents) in Template::DevPipeline.files() {
            if path.starts_with("schemas/") {
                for forbidden in ["$ref", "$defs", "oneOf", "anyOf", "allOf"] {
                    assert!(
                        !contents.contains(forbidden),
                        "{path} must not use {forbidden}: aion codegen rejects it"
                    );
                }
            }
        }
    }

    #[test]
    fn worker_manifests_exist_exactly_for_templates_with_activities() {
        for template in Template::all() {
            let has_worker = !template.worker_files().is_empty();
            let has_activities = !template.activities().is_empty();
            assert_eq!(
                has_worker,
                has_activities,
                "template {} worker manifest must match its activity surface",
                template.id()
            );
            let worker_files = template.worker_files();
            if !worker_files.is_empty() {
                let paths: Vec<&str> = worker_files.iter().map(|(path, _)| *path).collect();
                assert!(paths.contains(&"worker/Cargo.toml"));
                assert!(paths.contains(&"worker/src/main.rs"));
                let main = worker_files
                    .iter()
                    .find(|(path, _)| *path == "worker/src/main.rs")
                    .map(|(_, contents)| *contents)
                    .unwrap_or_default();
                // Whitespace-insensitive: registrations may be wrapped
                // across lines by rustfmt.
                let condensed: String = main.split_whitespace().collect();
                for activity in template.activities() {
                    assert!(
                        condensed.contains(&format!("register_activity(\"{activity}\"")),
                        "template {} worker must register {activity}",
                        template.id()
                    );
                }
            }
        }
    }
}
