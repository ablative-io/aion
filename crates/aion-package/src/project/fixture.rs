//! Synthetic-project fixtures shared by project packaging unit tests.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

type FixtureResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Creates a unique temporary directory populated with the given relative files.
pub(crate) fn temp_project(label: &str, files: &[(&str, &[u8])]) -> FixtureResult<PathBuf> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!("aion-project-{label}-{nanos}"));
    fs::create_dir_all(&root)?;
    for (relative, bytes) in files {
        write_file(&root, relative, bytes)?;
    }
    Ok(root)
}

/// Writes one file under `root`, creating parent directories.
pub(crate) fn write_file(root: &Path, relative: &str, bytes: &[u8]) -> FixtureResult<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, bytes)?;
    Ok(())
}

/// `gleam.toml` for the synthetic `demo` project: production dependencies
/// `aion_flow` and `dep_a`; `dev_only` is intentionally absent.
pub(crate) const DEMO_GLEAM_TOML: &str = r#"name = "demo"
version = "0.1.0"
target = "erlang"

[dependencies]
aion_flow = { path = "../aion_flow" }
dep_a = ">= 1.0.0"

[dev-dependencies]
dev_only = ">= 1.0.0"
"#;

/// Lockfile for the synthetic project: `dep_a` transitively requires `dep_b`,
/// and `dev_only` is locked but outside the production closure.
pub(crate) const DEMO_LOCKFILE: &str = r#"packages = [
  { name = "aion_flow", version = "0.1.0", requirements = [] },
  { name = "dep_a", version = "1.0.0", requirements = ["dep_b"] },
  { name = "dep_b", version = "1.0.0", requirements = [] },
  { name = "dev_only", version = "1.0.0", requirements = [] },
]

[requirements]
aion_flow = { path = "../aion_flow" }
dep_a = { version = ">= 1.0.0" }
"#;

/// Minimal valid `workflow.toml` for the synthetic project.
pub(crate) const DEMO_WORKFLOW_TOML: &str = r#"[[workflow]]
entry_module = "demo"
entry_function = "run"
timeout_seconds = 30
input_schema = "schemas/input.json"
output_schema = "schemas/output.json"
activities = ["greet"]
"#;

/// Builds a fully-populated synthetic built project and returns its root.
///
/// Layout: `gleam.toml` + lockfile, a `workflow.toml` with one `demo` entry,
/// JSON schema files, first-party sources `demo` and `demo/nested`, and a
/// `build/dev/erlang` tree containing the production closure (`demo`, `dep_a`,
/// `dep_b`, `aion_flow` with SDK test modules), a `dev_only` dev-dependency
/// package, a `fingerprint` directory without an ebin, and `.app` entries.
pub(crate) fn synthetic_built_project(label: &str) -> FixtureResult<PathBuf> {
    let root = temp_project(
        label,
        &[
            ("gleam.toml", DEMO_GLEAM_TOML.as_bytes()),
            ("manifest.toml", DEMO_LOCKFILE.as_bytes()),
            ("workflow.toml", DEMO_WORKFLOW_TOML.as_bytes()),
            ("schemas/input.json", br#"{ "type": "object" }"#),
            ("schemas/output.json", b"true"),
            ("src/demo.gleam", b"pub fn run() { Nil }"),
            ("src/demo/nested.gleam", b"pub fn helper() { Nil }"),
            ("src/notes.txt", b"not a source module"),
            ("build/dev/erlang/demo/ebin/demo.beam", b"demo-bytes"),
            (
                "build/dev/erlang/demo/ebin/demo@nested.beam",
                b"demo-nested-bytes",
            ),
            ("build/dev/erlang/demo/ebin/demo.app", b"app-resource"),
            (
                "build/dev/erlang/aion_flow/ebin/aion_flow.beam",
                b"aion-flow-bytes",
            ),
            (
                "build/dev/erlang/aion_flow/ebin/aion_flow_ffi.beam",
                b"sdk-double-bytes",
            ),
            (
                "build/dev/erlang/aion_flow/ebin/aion@testing.beam",
                b"sdk-testing-bytes",
            ),
            (
                "build/dev/erlang/aion_flow/ebin/aion@testing@mock.beam",
                b"sdk-mock-bytes",
            ),
            ("build/dev/erlang/dep_a/ebin/dep_a.beam", b"dep-a-bytes"),
            ("build/dev/erlang/dep_b/ebin/dep_b.beam", b"dep-b-bytes"),
            (
                "build/dev/erlang/dev_only/ebin/dev_only.beam",
                b"dev-only-bytes",
            ),
            (
                "build/dev/erlang/fingerprint/fingerprint.v1",
                b"not-a-package",
            ),
        ],
    )?;
    Ok(root)
}
