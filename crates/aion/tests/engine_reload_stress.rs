//! Concurrent load-during-dispatch stress for the #62 live-reload seam
//! (brief §4 tests 3 and 11): N parallel starters race M sequential loads
//! of distinct versions; every start must be internally consistent between
//! its recorded `package_version`, its registry pin, and its output.

#[path = "common/reload_fixture.rs"]
mod reload_fixture;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aion_core::PackageVersion;
use aion_store::{EventStore, InMemoryStore};

use reload_fixture::{
    RELOAD_MODULE, compile_reload_beam, engine_with, input, recorded_version, reload_package,
    result_int, start, version_of,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn parallel_starters_racing_sequential_loads_stay_version_consistent() -> TestResult {
    const STARTERS: usize = 4;
    const STARTS_PER_TASK: usize = 12;
    const VERSIONS: u32 = 4;

    let mut packages = Vec::new();
    for version in 1..=VERSIONS {
        packages.push(reload_package(&compile_reload_beam(version)?, "run")?);
    }
    let version_by_output: HashMap<i64, PackageVersion> = packages
        .iter()
        .enumerate()
        .map(|(index, package)| (i64::try_from(index).unwrap_or(0) + 1, version_of(package)))
        .collect();

    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = Arc::new(engine_with(&store, vec![packages[0].clone()]).await?);

    // N parallel starters...
    let mut starters = Vec::new();
    for _ in 0..STARTERS {
        let engine = Arc::clone(&engine);
        starters.push(tokio::spawn(async move {
            let mut runs = Vec::new();
            for _ in 0..STARTS_PER_TASK {
                let handle = engine
                    .start_workflow(RELOAD_MODULE, input()?, HashMap::new())
                    .await?;
                runs.push((
                    handle.workflow_id().clone(),
                    handle.run_id().clone(),
                    handle.loaded_version().clone(),
                ));
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(runs)
        }));
    }

    // ...racing M sequential loads of distinct versions.
    for package in packages.iter().skip(1) {
        tokio::time::sleep(Duration::from_millis(8)).await;
        engine.load_package(package.clone()).await?;
    }

    let mut total = 0;
    for starter in starters {
        let runs = starter.await?.map_err(|error| error.to_string())?;
        for (workflow_id, run_id, registry_version) in runs {
            total += 1;
            // Output, recorded pin, and registry pin all name one version.
            let output = result_int(&engine, &workflow_id, &run_id).await?;
            let expected = version_by_output
                .get(&output)
                .ok_or_else(|| format!("unknown version output {output}"))?;
            let history = store.read_history(&workflow_id).await?;
            let recorded = recorded_version(&history, &run_id)?;
            assert_eq!(
                &recorded, expected,
                "recorded package_version must match the executed code"
            );
            assert_eq!(
                PackageVersion::new(registry_version.to_string()),
                recorded,
                "the registry pin must match the recorded version"
            );
        }
    }
    assert_eq!(
        total,
        STARTERS * STARTS_PER_TASK,
        "every start must succeed"
    );

    // Every version stays loaded (loads never evict), the route points at
    // the last load, and no start pins leak once everything completed.
    let versions = engine.list_workflow_versions()?;
    assert_eq!(versions.len(), VERSIONS as usize);
    let route_active: Vec<_> = versions
        .iter()
        .filter(|info| info.route_active)
        .map(|info| PackageVersion::new(info.content_hash.to_string()))
        .collect();
    let last = packages
        .last()
        .map(version_of)
        .ok_or("missing last package")?;
    assert_eq!(route_active, vec![last]);

    engine.shutdown()?;
    Ok(())
}

/// Catalog snapshot-swap vs reader-clone stress (brief §4 test 11): starts
/// initiated strictly after a load returns must always resolve the new
/// version, for every flip in a rapid sequence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_flip_in_a_rapid_load_sequence_is_visible_to_subsequent_starts() -> TestResult {
    const VERSIONS: u32 = 6;
    let mut packages = Vec::new();
    for version in 1..=VERSIONS {
        packages.push(reload_package(&compile_reload_beam(version)?, "run")?);
    }
    let store: Arc<dyn EventStore> = Arc::new(InMemoryStore::default());
    let engine = Arc::new(engine_with(&store, vec![packages[0].clone()]).await?);

    for (index, package) in packages.iter().enumerate().skip(1) {
        engine.load_package(package.clone()).await?;
        let (id, run) = start(&engine).await?;
        let output = result_int(&engine, &id, &run).await?;
        assert_eq!(
            output,
            i64::try_from(index).unwrap_or(0) + 1,
            "a start after load_package returns must execute the new version"
        );
    }

    engine.shutdown()?;
    Ok(())
}
