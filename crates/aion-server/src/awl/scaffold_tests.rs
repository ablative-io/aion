use std::process::Command;

use super::scaffold::{ScaffoldRefusal, ScaffoldRequest, scaffold};

const SOURCE: &str =
    include_str!("../../../aion-awl/tests/fixtures/rev2/flagship/valid/awl_hello.awl");
const INLINE_SCHEMA: &str = include_str!(
    "../../../aion-awl/tests/fixtures/rev2/schema-doors/valid/inline_schema_round.awl"
);

fn request(runtime: &str) -> ScaffoldRequest {
    ScaffoldRequest {
        source: SOURCE.to_owned(),
        worker: "awl_hello".to_owned(),
        runtime: runtime.to_owned(),
    }
}

#[test]
fn shell_scaffold_is_stable_manifest_only_wiring() -> Result<(), Box<dyn std::error::Error>> {
    let response = scaffold(&request("shell"));
    let files = response.files.ok_or("shell scaffold was refused")?;
    assert_eq!(files.len(), 2);
    assert_eq!(
        files.get("worker.toml").ok_or("missing worker.toml")?,
        "# Generated wiring. Types, timeout, and retry live only in the .awl.\n[worker]\nname = \"awl_hello\"\ntask_queue = \"awl_hello\"\n\n[[action]]\nname = \"greet\"\ncommand = [\"printf\", \"%s\", \"{input}\"]\nresult = \"json\"\n\n[[action]]\nname = \"shout\"\ncommand = [\"printf\", \"%s\", \"{input}\"]\nresult = \"json\"\n"
    );
    assert!(!files.keys().any(|path| {
        std::path::Path::new(path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
    }));
    Ok(())
}

#[test]
fn rust_scaffold_projects_reachable_types_and_handlers() -> Result<(), Box<dyn std::error::Error>> {
    let response = scaffold(&request("rust"));
    let files = response.files.ok_or("Rust scaffold was refused")?;
    let main = files.get("src/main.rs").ok_or("missing main")?;
    assert!(main.contains("struct Greeting"));
    assert!(main.contains("fn greet("));
    assert!(main.contains("todo!(\"implement greet\")"));
    assert!(main.contains(".task_queue(\"awl_hello\")"));
    Ok(())
}

#[test]
fn scaffold_refusals_are_typed() {
    let mut unknown = request("rust");
    unknown.worker = "absent".to_owned();
    assert!(matches!(
        scaffold(&unknown).refusal,
        Some(ScaffoldRefusal::UnknownWorker { .. })
    ));
    assert!(matches!(
        scaffold(&request("python")).refusal,
        Some(ScaffoldRefusal::UnsupportedRuntime { .. })
    ));

    let schema = ScaffoldRequest {
        source: INLINE_SCHEMA.to_owned(),
        worker: "doors".to_owned(),
        runtime: "rust".to_owned(),
    };
    assert!(matches!(
        scaffold(&schema).refusal,
        Some(ScaffoldRefusal::UnscaffoldableActionType { action, r#type, .. })
            if action == "write_report" && r#type == "Round"
    ));
}

#[test]
fn generated_flagship_rust_worker_compiles() -> Result<(), Box<dyn std::error::Error>> {
    let response = scaffold(&request("rust"));
    let files = response.files.ok_or("Rust scaffold was refused")?;
    let directory = tempfile::tempdir()?;
    for (path, content) in files {
        let target = directory.path().join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = if target.file_name().is_some_and(|name| name == "Cargo.toml") {
            let worker = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../aion-worker")
                .canonicalize()?;
            content.replace(
                "aion-worker = \"0.8.0\"",
                &format!("aion-worker = {{ path = {worker:?} }}"),
            )
        } else {
            content
        };
        std::fs::write(target, content)?;
    }
    let output = Command::new(env!("CARGO"))
        .args(["check", "--quiet"])
        .current_dir(directory.path())
        .output()?;
    assert!(
        output.status.success(),
        "generated worker failed cargo check:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
