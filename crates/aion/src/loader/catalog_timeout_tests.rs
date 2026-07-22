//! Timeout-aware package identity tests through the real workflow catalog load path.

use std::time::Duration;

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, ExtractionLimits, Manifest, ManifestVersion,
    Package, PackageBuilder, WorkflowEntry,
};
use serde_json::json;

use super::WorkflowCatalog;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn additional_entry(workflow_type: &str, timeout: Option<Duration>) -> WorkflowEntry {
    WorkflowEntry {
        workflow_type: workflow_type.to_owned(),
        entry_module: "workflow/order".to_owned(),
        entry_function: format!("{workflow_type}_run"),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "object" }),
        timeout,
        internal: true,
    }
}

fn multi_entry_package(
    primary: Option<Duration>,
    additional: Vec<WorkflowEntry>,
    explicit: bool,
) -> Result<Package, aion_package::PackageError> {
    let beams = BeamSet::new(vec![BeamModule::new("workflow/order", vec![1, 2, 3])])?;
    let manifest = Manifest {
        entry_module: "workflow/order".to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "object" }),
        timeout: primary,
        activities: Vec::new(),
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: additional,
    };
    let mut builder = PackageBuilder::new(manifest, beams);
    if explicit {
        builder = builder.with_explicit_timeout_identity();
    }
    let archive = builder.write_to_bytes()?;
    Package::load_from_bytes(archive, ExtractionLimits::unbounded())
}

/// LAW 2, per-entry: a legacy (beams-only) identity carrying BOTH a primary and
/// an additional-entry timeout value arms nothing — every entry reads as
/// undeclared regardless of the values the manifest happens to carry.
#[test]
fn legacy_identity_with_additional_timeout_arms_no_entry() -> TestResult {
    let package = multi_entry_package(
        Some(Duration::from_secs(3_600)),
        vec![additional_entry(
            "order_child",
            Some(Duration::from_secs(3_600)),
        )],
        false,
    )?;
    assert!(!package.has_declared_timeout());
    assert_eq!(package.declared_timeout(), None);
    assert_eq!(
        package.declared_entry_timeout(Some(Duration::from_secs(3_600))),
        None,
        "an additional entry under a legacy identity is unarmed"
    );
    Ok(())
}

/// The per-entry timeout-bearing identity authenticates EVERY entry: a primary
/// and an additional entry, each with its own authored timeout, both read as
/// declared with their exact values.
#[test]
fn per_entry_identity_authenticates_primary_and_additional() -> TestResult {
    let package = multi_entry_package(
        Some(Duration::from_secs(60)),
        vec![additional_entry(
            "order_child",
            Some(Duration::from_secs(30)),
        )],
        true,
    )?;
    assert!(package.has_declared_timeout());
    assert_eq!(package.declared_timeout(), Some(Duration::from_secs(60)));
    assert_eq!(
        package.declared_entry_timeout(Some(Duration::from_secs(30))),
        Some(Duration::from_secs(30)),
        "an authenticated additional entry arms its own value"
    );
    Ok(())
}

fn package(timeout: Duration, explicit: bool) -> Result<Package, aion_package::PackageError> {
    let beams = BeamSet::new(vec![BeamModule::new("workflow/order", vec![1, 2, 3])])?;
    let manifest = Manifest {
        entry_module: "workflow/order".to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "object" }),
        timeout: Some(timeout),
        activities: Vec::new(),
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    };
    let mut builder = PackageBuilder::new(manifest, beams);
    if explicit {
        builder = builder.with_explicit_timeout_identity();
    }
    let archive = builder.write_to_bytes()?;
    Package::load_from_bytes(archive, ExtractionLimits::unbounded())
}

async fn load(
    catalog: &WorkflowCatalog,
    package: &Package,
) -> Result<crate::loader::LoadOutcome, crate::EngineError> {
    catalog
        .load_package_with(
            package,
            |_deployed_name, _bytes| Ok(()),
            |_deployed_name| Ok(()),
            |_entry, _function| Ok(()),
        )
        .await
}

#[tokio::test]
async fn timeout_only_redeploy_loads_as_a_new_version() -> TestResult {
    let original = package(Duration::from_secs(3_600), false)?;
    let edited = package(Duration::from_secs(7_200), true)?;
    assert_eq!(original.beams(), edited.beams());
    assert_ne!(original.content_hash(), edited.content_hash());

    let catalog = WorkflowCatalog::new();
    let first = load(&catalog, &original).await?;
    let second = load(&catalog, &edited).await?;
    assert!(first.freshly_loaded);
    assert!(second.freshly_loaded);
    assert_eq!(catalog.workflows()?.len(), 2);
    assert_eq!(
        catalog
            .routed("workflow/order")?
            .as_ref()
            .map(crate::loader::LoadedWorkflow::version),
        Some(edited.content_hash())
    );
    Ok(())
}
