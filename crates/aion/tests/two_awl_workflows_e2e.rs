//! Two same-shape AWL workflows package, deploy, and restart without child-type collisions.

#[path = "test_support/gleam.rs"]
mod gleam_test_support;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use aion::signal::ConcreteSignalRouter;
use aion::{Engine, EngineBuilder, RuntimeHandle, SignalRouter};
use aion_package::{PackageOptions, package_project};
use aion_store::{EventStore, InMemoryStore};

const TEMPLATE: &str = r"
//! Same-shape package collision proof.
workflow PARENT
  input items: [String]
  outcome done: type Done, route success

type Done { count: Int }

worker proof
  action first(item: String) -> String
  action second(item: String) -> String

step fan
  distribute item in items
step one
  first(item: item) -> prepared
step two
  second(item: prepared) -> result
step gather
  collect result -> results
  results |> count -> total
  route done(count: total)
";

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn repo_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot resolve repository root".into())
}

async fn engine(store: &Arc<dyn EventStore>) -> Result<Engine, Box<dyn std::error::Error>> {
    Ok(EngineBuilder::new()
        .store_arc(Arc::clone(store))
        .in_memory_visibility()
        .scheduler_threads(2)
        .signal_router_factory(|runtime: Arc<RuntimeHandle>, handoff| {
            Arc::new(ConcreteSignalRouter::new(runtime, handoff)) as Arc<dyn SignalRouter>
        })
        .build()
        .await?)
}

#[tokio::test]
async fn same_shape_workflows_package_deploy_and_restart_together() -> TestResult {
    if crate::gleam_test_support::skip_if_unavailable() {
        return Ok(());
    }
    let repo = repo_root()?;
    let root = repo.join("target/flow-vocab-b5-two-workflows");
    fs::create_dir_all(root.join("src"))?;
    fs::create_dir_all(root.join("schemas"))?;
    let mut descriptor = String::new();
    let mut expected_types = BTreeSet::new();
    let mut child_types = Vec::new();

    for parent in ["alpha_parallel", "beta_parallel"] {
        let document = aion_awl::parse(&TEMPLATE.replace("PARENT", parent))?;
        let diagnostics = aion_awl::check(&document);
        if !diagnostics.is_empty() {
            return Err(format!("{parent} did not check: {diagnostics:?}").into());
        }
        let artifact = aion_awl::emit_artifact(&document)?;
        let [child] = artifact.synthesized_workflows.as_slice() else {
            return Err(format!("{parent} did not emit exactly one child").into());
        };
        expected_types.insert(parent.to_owned());
        expected_types.insert(child.workflow_type.clone());
        child_types.push(child.workflow_type.clone());
        fs::write(
            root.join("src").join(format!("{parent}.gleam")),
            &artifact.source,
        )?;
        fs::write(
            root.join("src").join(format!("{parent}.awl.json")),
            serde_json::to_vec_pretty(&artifact.project_metadata())?,
        )?;
        fs::write(
            root.join("schemas").join(format!("{parent}-input.json")),
            serde_json::to_vec_pretty(&aion_awl::schema_for_workflow(&document)?)?,
        )?;
        fs::write(
            root.join("schemas").join(format!("{parent}-output.json")),
            serde_json::to_vec_pretty(&aion_awl::schema_for_outcomes(&document)?)?,
        )?;
        write!(
            descriptor,
            "[[workflow]]\nentry_module = \"{parent}\"\nentry_function = \"run\"\n\
             timeout_seconds = 30\ninput_schema = \"schemas/{parent}-input.json\"\n\
             output_schema = \"schemas/{parent}-output.json\"\n\
             activities = [\"first\", \"second\"]\n\n"
        )?;
    }
    assert_ne!(child_types[0], child_types[1]);
    fs::write(root.join("workflow.toml"), descriptor)?;
    fs::write(
        root.join("gleam.toml"),
        format!(
            "name = \"b5_two_workflows\"\nversion = \"1.0.0\"\ntarget = \"erlang\"\n\n\
             [dependencies]\ngleam_stdlib = \">= 0.44.0 and < 3.0.0\"\n\
             gleam_json = \">= 3.0.0 and < 4.0.0\"\n\
             aion_flow = {{ path = \"{}\" }}\n",
            repo.join("gleam/aion_flow").display()
        ),
    )?;
    let build = Command::new("gleam")
        .arg("build")
        .current_dir(&root)
        .output()?;
    if !build.status.success() {
        return Err(format!(
            "two-workflow project did not build:\n{}\n{}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        )
        .into());
    }
    let report = package_project(&root, &PackageOptions::default())?;
    assert_eq!(report.packages.len(), 2);

    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let first_epoch = engine(&store).await?;
    for packaged in report.packages {
        first_epoch.load_package(packaged.package).await?;
    }
    first_epoch.shutdown()?;

    let recovered = engine(&store).await?;
    let recovered_types = recovered
        .list_workflow_versions()?
        .into_iter()
        .map(|entry| entry.workflow_type)
        .collect::<BTreeSet<_>>();
    assert_eq!(recovered_types, expected_types);
    recovered.shutdown()?;
    Ok(())
}
