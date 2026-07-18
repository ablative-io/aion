//! Catalog load/route/pin unit tests (ported from the dissolved
//! `LoadedWorkflows` suite, plus route-pointer and start-pin coverage).

use std::{cell::RefCell, collections::BTreeMap, time::Duration};

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder, PackageError, content_hash, deployed_name,
    parse_deployed_name,
};
use serde_json::json;

use super::WorkflowCatalog;
use crate::EngineError;
use crate::runtime::{RuntimeConfig, RuntimeHandle, RuntimeInput};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn manifest(entry_module: &str) -> Manifest {
    Manifest {
        entry_module: entry_module.to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "object" }),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "activity/send".to_owned(),
        }],
        version: ManifestVersion::new("placeholder"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    }
}

fn package(entry_module: &str, entry_bytes: Vec<u8>) -> Result<Package, PackageError> {
    let beams = BeamSet::new(vec![
        BeamModule::new("workflow/support", vec![4, 5, 6]),
        BeamModule::new(entry_module, entry_bytes),
    ])?;
    let bytes = PackageBuilder::with_source(
        manifest(entry_module),
        beams,
        BTreeMap::<String, Vec<u8>>::new(),
    )
    .write_to_bytes()?;
    Package::load_from_bytes(bytes, ExtractionLimits::unbounded())
}

fn entry_only_package(entry_module: &str, bytes: Vec<u8>) -> Result<Package, PackageError> {
    let beams = BeamSet::new(vec![BeamModule::new(entry_module, bytes)])?;
    let archive = PackageBuilder::new(manifest(entry_module), beams).write_to_bytes()?;
    Package::load_from_bytes(archive, ExtractionLimits::unbounded())
}

fn fixture_workflow_beam() -> &'static [u8] {
    include_bytes!("../../tests/fixtures/aion_fixture_workflow.beam")
}

fn fixture_workflow_package() -> Result<Package, PackageError> {
    let mut manifest = manifest("aion_fixture_workflow");
    manifest.entry_function = "complete".to_owned();
    let beams = BeamSet::new(vec![BeamModule::new(
        "aion_fixture_workflow",
        fixture_workflow_beam().to_vec(),
    )])?;
    let archive = PackageBuilder::new(manifest, beams).write_to_bytes()?;
    Package::load_from_bytes(archive, ExtractionLimits::unbounded())
}

async fn load_counting(
    catalog: &WorkflowCatalog,
    package: &Package,
    registered: &RefCell<Vec<String>>,
) -> Result<crate::loader::LoadOutcome, EngineError> {
    catalog
        .load_package_with(
            package,
            |deployed_name, _bytes| {
                registered.borrow_mut().push(deployed_name.to_owned());
                Ok(())
            },
            |_deployed_name| Ok(()),
            |_entry, _function| Ok(()),
        )
        .await
}

async fn load_plain(
    catalog: &WorkflowCatalog,
    package: &Package,
) -> Result<crate::loader::LoadOutcome, EngineError> {
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
async fn registers_every_package_derived_deployed_module() -> TestResult {
    let package = package("workflow/order", vec![1, 2, 3])?;
    let registered = RefCell::new(Vec::<String>::new());
    let catalog = WorkflowCatalog::new();

    let outcome = load_counting(&catalog, &package, &registered).await?;
    let record = outcome.record;
    assert!(outcome.freshly_loaded, "first load must be fresh");
    assert!(outcome.route_changed, "first load must take the route");

    let registered = registered.into_inner();
    let expected: Vec<String> = package
        .deployed_modules()
        .into_iter()
        .map(|(name, _bytes)| name)
        .collect();
    assert_eq!(registered, expected);
    for deployed_name in registered {
        let parsed = parse_deployed_name(&deployed_name)?;
        assert_eq!(parsed.hash(), package.content_hash());
        assert!(package.beams().get(parsed.logical()).is_some());
        assert!(catalog.has_registered_module(&deployed_name));
    }
    assert_eq!(record.workflow_type(), "workflow/order");
    Ok(())
}

#[tokio::test]
async fn records_deployed_entry_function_and_routes_to_it() -> TestResult {
    let package = package("workflow/order", vec![1, 2, 3])?;
    let catalog = WorkflowCatalog::new();

    let record = load_plain(&catalog, &package).await?.record;

    assert_eq!(record.workflow_type(), package.manifest().entry_module);
    assert_eq!(
        record.deployed_entry_module(),
        deployed_name(&package.manifest().entry_module, package.content_hash())
    );
    assert_eq!(record.entry_function(), package.manifest().entry_function);
    assert_eq!(record.version(), package.content_hash());
    assert_eq!(catalog.routed("workflow/order")?, Some(record.clone()));
    assert_eq!(
        catalog.get("workflow/order", package.content_hash())?,
        Some(record)
    );
    Ok(())
}

#[tokio::test]
async fn retains_two_versions_and_routes_to_the_last_loaded() -> TestResult {
    let first = package("workflow/order", vec![1, 2, 3])?;
    let second = package("workflow/order", vec![1, 2, 4])?;
    let catalog = WorkflowCatalog::new();

    let first_record = load_plain(&catalog, &first).await?.record;
    let second_record = load_plain(&catalog, &second).await?.record;

    assert_ne!(first.content_hash(), second.content_hash());
    assert_ne!(
        first_record.deployed_entry_module(),
        second_record.deployed_entry_module()
    );
    assert!(catalog.has_registered_module(first_record.deployed_entry_module()));
    assert!(catalog.has_registered_module(second_record.deployed_entry_module()));
    assert_eq!(
        catalog.get("workflow/order", first.content_hash())?,
        Some(first_record)
    );
    assert_eq!(
        catalog.get("workflow/order", second.content_hash())?,
        Some(second_record.clone())
    );
    assert_eq!(catalog.workflows()?.len(), 2);
    // The route pointer follows the most recent load.
    assert_eq!(catalog.routed("workflow/order")?, Some(second_record));
    Ok(())
}

#[tokio::test]
async fn identical_reload_is_idempotent_and_reload_re_routes() -> TestResult {
    let first = package("workflow/order", vec![1, 2, 3])?;
    let second = package("workflow/order", vec![1, 2, 4])?;
    let calls = RefCell::new(Vec::<String>::new());
    let catalog = WorkflowCatalog::new();

    let first_outcome = load_counting(&catalog, &first, &calls).await?;
    assert!(first_outcome.freshly_loaded);
    assert!(first_outcome.route_changed);
    let first_record = first_outcome.record;
    let after_first = calls.borrow().len();
    let again = load_counting(&catalog, &first, &calls).await?;
    assert_eq!(first_record, again.record);
    assert!(!again.freshly_loaded, "re-load must report idempotence");
    assert!(
        !again.route_changed,
        "re-loading the route-active hash is a full no-op"
    );
    assert_eq!(
        calls.borrow().len(),
        after_first,
        "re-load must register nothing"
    );
    assert_eq!(catalog.workflows()?.len(), 1);

    // Load v2 (route moves), then re-deploy v1: the rolled-back hash must
    // take the route again without any new registration.
    load_counting(&catalog, &second, &calls).await?;
    assert_eq!(
        catalog
            .routed("workflow/order")?
            .map(|w| w.version().clone()),
        Some(second.content_hash().clone())
    );
    let before = calls.borrow().len();
    let re_deployed = load_counting(&catalog, &first, &calls).await?;
    assert_eq!(re_deployed.record, first_record);
    assert!(
        !re_deployed.freshly_loaded,
        "re-deploy of a resident hash registers nothing"
    );
    assert!(
        re_deployed.route_changed,
        "re-deploying a rolled-back hash must re-point the route"
    );
    assert_eq!(calls.borrow().len(), before);
    assert_eq!(
        catalog
            .routed("workflow/order")?
            .map(|w| w.version().clone()),
        Some(first.content_hash().clone())
    );
    Ok(())
}

#[tokio::test]
async fn missing_entry_module_returns_load_error() -> TestResult {
    let package = package("workflow/order", vec![1, 2, 3])?;
    let missing = package_with_missing_entry(&package, "workflow/missing");
    let catalog = WorkflowCatalog::new();

    let result = load_plain(&catalog, &missing).await;

    assert!(
        matches!(&result, Err(EngineError::Load { reason }) if reason.contains("workflow/missing")),
        "missing entry should fail with EngineError::Load"
    );
    assert_eq!(catalog.workflows()?.len(), 0);
    assert!(!catalog.has_registered_module(&missing.deployed_entry_module()));
    Ok(())
}

#[tokio::test]
async fn collision_from_different_hash_fails_before_registration() -> TestResult {
    let first = entry_only_package("workflow/order", vec![1, 2, 3])?;
    let second = entry_only_package("workflow/order", vec![1, 2, 4])?;
    let colliding_name = first.deployed_entry_module();
    let calls = RefCell::new(Vec::<String>::new());
    let catalog = WorkflowCatalog::new();
    catalog.note_registered_module(colliding_name.clone(), second.content_hash().clone())?;

    let result = load_counting(&catalog, &first, &calls).await;

    let expected_hash = first.content_hash().to_string();
    assert!(
        matches!(&result, Err(EngineError::Load { reason })
            if reason.contains(&colliding_name) && reason.contains(&expected_hash)),
        "different hash collision should fail with EngineError::Load"
    );
    assert!(calls.borrow().is_empty());
    assert_eq!(catalog.workflows()?.len(), 0);
    Ok(())
}

#[tokio::test]
async fn runtime_failure_does_not_commit_catalog_state() -> TestResult {
    let package = package("workflow/order", vec![1, 2, 3])?;
    let catalog = WorkflowCatalog::new();

    let result = catalog
        .load_package_with(
            &package,
            |_deployed_name, _bytes| {
                Err(EngineError::Runtime {
                    reason: "boom".to_owned(),
                })
            },
            |_deployed_name| Ok(()),
            |_entry, _function| Ok(()),
        )
        .await;

    assert!(
        matches!(&result, Err(EngineError::Load { reason }) if reason.contains("boom")),
        "runtime failure should fail load with EngineError::Load"
    );
    assert_eq!(catalog.workflows()?.len(), 0);
    assert_eq!(catalog.routed("workflow/order")?, None);
    for (deployed_name, _bytes) in package.deployed_modules() {
        assert!(!catalog.has_registered_module(&deployed_name));
    }
    Ok(())
}

#[tokio::test]
async fn entry_verification_failure_rolls_back_every_registration() -> TestResult {
    let package = package("workflow/order", vec![1, 2, 3])?;
    let rolled_back = RefCell::new(Vec::<String>::new());
    let catalog = WorkflowCatalog::new();

    let result = catalog
        .load_package_with(
            &package,
            |_deployed_name, _bytes| Ok(()),
            |deployed_name| {
                rolled_back.borrow_mut().push(deployed_name.to_owned());
                Ok(())
            },
            |entry, function| {
                Err(EngineError::Load {
                    reason: format!("`{entry}` does not export `{function}`"),
                })
            },
        )
        .await;

    assert!(
        matches!(&result, Err(EngineError::Load { reason })
            if reason.contains("entry verification failed") && reason.contains("does not export")),
        "entry verification must fail the load: {result:?}"
    );
    let expected: Vec<String> = package
        .deployed_modules()
        .into_iter()
        .rev()
        .map(|(name, _bytes)| name)
        .collect();
    assert_eq!(rolled_back.into_inner(), expected);
    assert_eq!(catalog.workflows()?.len(), 0);
    assert_eq!(catalog.routed("workflow/order")?, None);
    Ok(())
}

#[tokio::test]
async fn package_loaded_under_content_hash_namespace_spawns_entrypoint() -> TestResult {
    let package = fixture_workflow_package()?;
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let catalog = WorkflowCatalog::new();

    let record = catalog.load_package(&runtime, &package).await?.record;
    let pid = runtime.spawn_workflow(
        record.deployed_entry_module(),
        record.entry_function(),
        RuntimeInput::default(),
    )?;
    let (reason, result) = runtime.process_exit_for_test(pid)?;

    assert_eq!(reason, beamr::process::ExitReason::Normal);
    assert_eq!(result, beamr::term::Term::small_int(42));
    runtime.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn unexported_entry_function_fails_the_runtime_load() -> TestResult {
    let mut manifest = manifest("aion_fixture_workflow");
    manifest.entry_function = "not_exported".to_owned();
    let beams = BeamSet::new(vec![BeamModule::new(
        "aion_fixture_workflow",
        fixture_workflow_beam().to_vec(),
    )])?;
    let archive = PackageBuilder::new(manifest, beams).write_to_bytes()?;
    let package = Package::load_from_bytes(archive, ExtractionLimits::unbounded())?;
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    let catalog = WorkflowCatalog::new();

    let result = catalog.load_package(&runtime, &package).await;

    assert!(
        matches!(&result, Err(EngineError::Load { reason }) if reason.contains("not_exported")),
        "unexported entry function must fail the load: {result:?}"
    );
    assert_eq!(catalog.workflows()?.len(), 0);
    assert!(!runtime.has_registered_module(&package.deployed_entry_module()));
    runtime.shutdown()?;
    Ok(())
}

#[tokio::test]
async fn start_pins_are_held_until_dropped() -> TestResult {
    let package = package("workflow/order", vec![1, 2, 3])?;
    let catalog = WorkflowCatalog::new();
    let record = load_plain(&catalog, &package).await?.record;
    let version = record.version().clone();

    assert!(!catalog.has_pinned_starts("workflow/order", &version)?);
    let routed = catalog
        .resolve_routed("workflow/order")?
        .ok_or("routed resolution missing")?;
    let exact = catalog
        .resolve_exact("workflow/order", &version)?
        .ok_or("exact resolution missing")?;
    assert_eq!(routed.workflow(), exact.workflow());
    assert!(catalog.has_pinned_starts("workflow/order", &version)?);

    drop(routed);
    assert!(
        catalog.has_pinned_starts("workflow/order", &version)?,
        "the second pin must keep the version pinned"
    );
    drop(exact);
    assert!(!catalog.has_pinned_starts("workflow/order", &version)?);
    Ok(())
}

#[tokio::test]
async fn versions_listing_reports_route_flags_sorted() -> TestResult {
    let first = package("workflow/order", vec![1, 2, 3])?;
    let second = package("workflow/order", vec![1, 2, 4])?;
    let catalog = WorkflowCatalog::new();
    load_plain(&catalog, &first).await?;
    load_plain(&catalog, &second).await?;

    let versions = catalog.versions()?;
    assert_eq!(versions.len(), 2);
    assert!(
        versions
            .iter()
            .all(|info| info.workflow_type == "workflow/order")
    );
    let active: Vec<bool> = versions.iter().map(|info| info.route_active).collect();
    assert_eq!(active.iter().filter(|flag| **flag).count(), 1);
    let routed = versions
        .iter()
        .find(|info| info.route_active)
        .ok_or("no route-active version")?;
    assert_eq!(&routed.content_hash, second.content_hash());
    Ok(())
}

fn package_with_missing_entry(original: &Package, missing_entry: &str) -> Package {
    let mut manifest = original.manifest().clone();
    manifest.entry_module = missing_entry.to_owned();
    Package::from_validated_parts_for_test(
        manifest,
        original.beams().clone(),
        BTreeMap::new(),
        original.content_hash().clone(),
    )
}

#[test]
fn content_hash_fixture_changes_when_bytes_change() -> Result<(), PackageError> {
    let first = BeamSet::new(vec![BeamModule::new("workflow/order", vec![1, 2, 3])])?;
    let second = BeamSet::new(vec![BeamModule::new("workflow/order", vec![1, 2, 4])])?;
    assert_ne!(content_hash(&first), content_hash(&second));
    Ok(())
}

#[tokio::test]
async fn route_version_re_points_and_rejects_unknown_hashes() -> TestResult {
    let first = package("workflow/order", vec![1, 2, 3])?;
    let second = package("workflow/order", vec![1, 2, 4])?;
    let catalog = WorkflowCatalog::new();
    let first_record = load_plain(&catalog, &first).await?.record;
    load_plain(&catalog, &second).await?;

    catalog
        .route_version("workflow/order", first.content_hash())
        .await?;
    assert_eq!(catalog.routed("workflow/order")?, Some(first_record));

    let unknown = aion_package::ContentHash::from_bytes([7; 32]);
    let result = catalog.route_version("workflow/order", &unknown).await;
    assert!(
        matches!(&result, Err(EngineError::UnknownVersion { workflow_type, version, loaded })
            if workflow_type == "workflow/order"
                && version == &unknown
                && loaded.contains(&first.content_hash().to_string())),
        "unknown route target must fail typed naming the loaded set: {result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn swap_out_refuses_routed_package_and_restore_round_trips() -> TestResult {
    let first = package("workflow/order", vec![1, 2, 3])?;
    let second = package("workflow/order", vec![1, 2, 4])?;
    let catalog = WorkflowCatalog::new();
    let first_record = load_plain(&catalog, &first).await?.record;
    load_plain(&catalog, &second).await?;

    {
        let _mutation = catalog.begin_mutation().await;
        let routed = catalog.swap_out_package("workflow/order", second.content_hash());
        assert!(
            matches!(&routed, Err(EngineError::RouteActive { workflow_type, version })
                if workflow_type == "workflow/order"
                    && version.to_string() == second.content_hash().to_string()),
            "swapping out the routed version must be refused: {routed:?}"
        );

        let removed = catalog.swap_out_package("workflow/order", first.content_hash())?;
        assert_eq!(catalog.get("workflow/order", first.content_hash())?, None);
        assert!(!catalog.has_registered_module(first_record.deployed_entry_module()));

        catalog.restore_package(removed)?;
    }
    assert_eq!(
        catalog.get("workflow/order", first.content_hash())?,
        Some(first_record)
    );
    Ok(())
}

/// D10 rider: an idempotent re-load that carries the resident content hash
/// with a DIFFERENT manifest must be refused typed, leaving the catalog
/// bit-for-bit untouched. The content hash covers beams only, so this is the
/// only thing standing between a remote deploy endpoint and a silent
/// wrong-deploy.
#[tokio::test]
async fn same_hash_different_manifest_reload_is_refused_typed() -> TestResult {
    let original = package("workflow/order", vec![1, 2, 3])?;
    let mut diverged_manifest = original.manifest().clone();
    diverged_manifest.entry_function = "start".to_owned();
    let archive = PackageBuilder::with_source(
        diverged_manifest,
        original.beams().clone(),
        BTreeMap::<String, Vec<u8>>::new(),
    )
    .write_to_bytes()?;
    let diverged = Package::load_from_bytes(archive, ExtractionLimits::unbounded())?;
    assert_eq!(
        original.content_hash(),
        diverged.content_hash(),
        "identical beams must carry the identical content hash"
    );

    let catalog = WorkflowCatalog::new();
    let resident = load_plain(&catalog, &original).await?;

    let result = load_plain(&catalog, &diverged).await;
    assert!(
        matches!(&result, Err(EngineError::ManifestMismatch { workflow_type, version, .. })
            if workflow_type == "workflow/order" && version == original.content_hash()),
        "diverged-manifest re-load must be refused typed: {result:?}"
    );

    // The refusal left the catalog untouched: one version, original routed.
    assert_eq!(catalog.workflows()?.len(), 1);
    assert_eq!(catalog.routed("workflow/order")?, Some(resident.record));

    // The byte-identical archive still re-loads idempotently.
    let again = load_plain(&catalog, &original).await?;
    assert!(!again.freshly_loaded);
    Ok(())
}
