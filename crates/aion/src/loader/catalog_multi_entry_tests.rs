//! Atomic same-package multi-entry catalog registration regressions.

use std::time::Duration;

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, ExtractionLimits, Manifest, ManifestVersion,
    Package, PackageBuilder, PackageError, WorkflowEntry,
};
use serde_json::json;

use super::WorkflowCatalog;
use crate::EngineError;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn multi_entry_package() -> Result<Package, PackageError> {
    let manifest = Manifest {
        entry_module: "aion_fixture_workflow".to_owned(),
        entry_function: "complete".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "object" }),
        timeout: Some(Duration::from_secs(30)),
        activities: Vec::new(),
        version: ManifestVersion::new("placeholder"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: vec![WorkflowEntry {
            workflow_type: "awl_distribute_items_0".to_owned(),
            entry_module: "aion_fixture_workflow".to_owned(),
            entry_function: "child_complete".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "string" }),
            timeout: Some(Duration::from_secs(10)),
            internal: true,
        }],
    };
    let beams = BeamSet::new(vec![BeamModule::new(
        "aion_fixture_workflow",
        include_bytes!("../../tests/fixtures/aion_fixture_workflow.beam").to_vec(),
    )])?;
    let bytes = PackageBuilder::new(manifest, beams).write_to_bytes()?;
    Package::load_from_bytes(bytes, ExtractionLimits::unbounded())
}

#[tokio::test]
async fn multi_entry_package_registers_and_routes_atomically() -> TestResult {
    let package = multi_entry_package()?;
    let catalog = WorkflowCatalog::new();
    let verified = std::cell::RefCell::new(Vec::new());
    catalog
        .load_package_with(
            &package,
            |_name, _bytes| Ok(()),
            |_name| Ok(()),
            |module, function| {
                verified
                    .borrow_mut()
                    .push((module.to_owned(), function.to_owned()));
                Ok(())
            },
        )
        .await?;

    let parent = catalog
        .get("aion_fixture_workflow", package.content_hash())?
        .ok_or("parent entry was not registered")?;
    let child = catalog
        .get("awl_distribute_items_0", package.content_hash())?
        .ok_or("child entry was not registered")?;
    assert_eq!(parent.version(), child.version());
    assert_eq!(
        parent.deployed_entry_module(),
        child.deployed_entry_module()
    );
    assert_eq!(child.entry_function(), "child_complete");
    assert_eq!(verified.borrow().len(), 2);
    assert!(catalog.routed("aion_fixture_workflow")?.is_some());
    assert!(catalog.routed("awl_distribute_items_0")?.is_some());
    Ok(())
}

#[tokio::test]
async fn secondary_entry_verification_failure_commits_no_routes() -> TestResult {
    let package = multi_entry_package()?;
    let catalog = WorkflowCatalog::new();
    let result = catalog
        .load_package_with(
            &package,
            |_name, _bytes| Ok(()),
            |_name| Ok(()),
            |_module, function| {
                if function == "child_complete" {
                    Err(EngineError::Load {
                        reason: "missing child".to_owned(),
                    })
                } else {
                    Ok(())
                }
            },
        )
        .await;
    assert!(result.is_err());
    assert!(catalog.workflows()?.is_empty());
    Ok(())
}

#[tokio::test]
async fn swapping_out_one_member_removes_and_restores_the_whole_archive_group() -> TestResult {
    let package = multi_entry_package()?;
    let catalog = WorkflowCatalog::new();
    catalog
        .load_package_with(
            &package,
            |_name, _bytes| Ok(()),
            |_name| Ok(()),
            |_module, _function| Ok(()),
        )
        .await?;

    let mut inactive = (*catalog.current()?).clone();
    inactive.routed.clear();
    catalog.install(inactive)?;
    let mutation_guard = catalog.begin_mutation().await;
    let removed = catalog.swap_out_package("aion_fixture_workflow", package.content_hash())?;
    assert_eq!(removed.workflow_types().count(), 2);
    assert!(
        catalog
            .get("aion_fixture_workflow", package.content_hash())?
            .is_none()
    );
    assert!(
        catalog
            .get("awl_distribute_items_0", package.content_hash())?
            .is_none()
    );
    assert_eq!(catalog.versions()?.len(), 0);

    catalog.restore_package(removed)?;
    assert_eq!(catalog.versions()?.len(), 2);
    assert!(
        catalog
            .get("awl_distribute_items_0", package.content_hash())?
            .is_some()
    );
    drop(mutation_guard);
    Ok(())
}

#[tokio::test]
async fn staged_group_is_not_resolvable_until_durable_publication() -> TestResult {
    let package = multi_entry_package()?;
    let catalog = WorkflowCatalog::new();
    let outcome = catalog
        .stage_package_with(
            &package,
            |_name, _bytes| Ok(()),
            |_name| Ok(()),
            |_module, _function| Ok(()),
        )
        .await?;
    assert!(outcome.freshly_loaded);
    assert!(catalog.routed("aion_fixture_workflow")?.is_none());
    assert!(catalog.routed("awl_distribute_items_0")?.is_none());
    assert!(
        catalog.resolve_routed("aion_fixture_workflow")?.is_none(),
        "a racing start must not resolve a staged, not-yet-durable hash"
    );

    catalog
        .publish_package_routes("aion_fixture_workflow", package.content_hash())
        .await?;
    assert_eq!(
        catalog
            .routed("aion_fixture_workflow")?
            .ok_or("published parent route missing")?
            .version(),
        package.content_hash()
    );
    assert_eq!(
        catalog
            .routed("awl_distribute_items_0")?
            .ok_or("published child route missing")?
            .version(),
        package.content_hash()
    );
    Ok(())
}
