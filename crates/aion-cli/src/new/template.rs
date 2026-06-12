//! Embedded scaffold templates for `aion new`.
//!
//! Each template is a hand-rolled manifest of `(target path, contents)`
//! pairs embedded with `include_str!` — no templating engine, no extra
//! dependency. Target paths and contents may carry the `{{name}}`
//! placeholder; the worker manifest additionally carries
//! `{{aion_worker_version}}`. Substitution lives in
//! [`crate::new::scaffold`].

use clap::ValueEnum;

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
}

/// Files every template emits.
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

impl Template {
    /// The kebab-case template name used by `--template`, `--help`, and the
    /// JSON output.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::HelloWorld => "hello-world",
            Self::ApprovalFlow => "approval-flow",
            Self::Saga => "saga",
        }
    }

    /// Every project file this template emits, shared files first.
    #[must_use]
    pub fn files(self) -> Vec<ManifestFile> {
        let mut files = SHARED_FILES.to_vec();
        files.extend_from_slice(match self {
            Self::HelloWorld => HELLO_WORLD_FILES,
            Self::ApprovalFlow => APPROVAL_FLOW_FILES,
            Self::Saga => SAGA_FILES,
        });
        files
    }

    /// The activities this template's workflow dispatches to a worker.
    #[must_use]
    pub fn activities(self) -> &'static [&'static str] {
        match self {
            Self::HelloWorld | Self::ApprovalFlow => &[],
            Self::Saga => &["charge_payment", "refund_payment"],
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
        }
    }

    /// All templates, for manifest-completeness tests.
    #[cfg(test)]
    pub fn all() -> &'static [Self] {
        &[Self::HelloWorld, Self::ApprovalFlow, Self::Saga]
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
                "backend = \"libsql\"",
                "query_timeout_ms",
                "event_broadcast_capacity",
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
                for activity in template.activities() {
                    assert!(
                        main.contains(&format!("register_activity(\"{activity}\"")),
                        "template {} worker must register {activity}",
                        template.id()
                    );
                }
            }
        }
    }
}
