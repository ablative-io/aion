//! Timeout-aware package identity tests through the real workflow catalog load path.

use std::time::Duration;

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, ExtractionLimits, Manifest, ManifestVersion,
    Package, PackageBuilder,
};
use serde_json::json;

use super::WorkflowCatalog;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn package(timeout: Duration, explicit: bool) -> Result<Package, aion_package::PackageError> {
    let beams = BeamSet::new(vec![BeamModule::new("workflow/order", vec![1, 2, 3])])?;
    let manifest = Manifest {
        entry_module: "workflow/order".to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "object" }),
        timeout,
        activities: Vec::new(),
        version: ManifestVersion::new("unstamped"),
        format_version: CURRENT_FORMAT_VERSION,
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
