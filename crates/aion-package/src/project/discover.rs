//! Compiled-module and first-party-source discovery for built Gleam projects.
//!
//! Discovery reads `build/dev/erlang` but ships only the production dependency
//! closure: the root package plus the transitive closure of `gleam.toml`'s
//! `[dependencies]` over the `manifest.toml` lockfile. Dev-dependency packages
//! and the SDK's test-only modules are excluded and reported, never silently
//! dropped from user-owned packages.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use super::{
    assemble::{ExcludedModule, ExcludedReason},
    error::PackagingError,
};
use crate::BeamModule;

/// Gleam package whose ebin legitimately contains SDK test machinery.
const SDK_PACKAGE: &str = "aion_flow";

/// Gleam project descriptor file name.
const GLEAM_CONFIG_FILE: &str = "gleam.toml";

/// Gleam lockfile name (unrelated to the `.aion` package manifest).
const GLEAM_LOCKFILE: &str = "manifest.toml";

#[derive(Debug, Deserialize)]
struct GleamConfig {
    name: String,
    #[serde(default)]
    dependencies: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
struct GleamLockfile {
    #[serde(default)]
    packages: Vec<LockedPackage>,
}

#[derive(Debug, Deserialize)]
struct LockedPackage {
    name: String,
    #[serde(default)]
    requirements: Vec<String>,
}

/// Compiled modules discovered from a built project, with exclusion provenance.
#[derive(Debug)]
pub(crate) struct DiscoveredModules {
    /// Production-closure modules ready for `BeamSet` construction.
    pub(crate) modules: Vec<BeamModule>,
    /// Modules excluded by the SDK test filter or the dependency-closure filter.
    pub(crate) excluded: Vec<ExcludedModule>,
    /// Compiled-output directory that was searched (`build/dev/erlang`).
    pub(crate) searched: PathBuf,
}

/// Discovers the production-closure compiled modules of a built Gleam project.
pub(crate) fn discover_modules(root: &Path) -> Result<DiscoveredModules, PackagingError> {
    let closure = production_closure(root)?;
    let searched = root.join("build").join("dev").join("erlang");
    if !searched.is_dir() {
        return Err(PackagingError::ProjectNotBuilt { missing: searched });
    }

    let mut modules = Vec::new();
    let mut excluded = Vec::new();
    record_dev_dependency_exclusions(&searched, &closure, &mut excluded)?;

    let mut provenance: BTreeMap<String, String> = BTreeMap::new();
    for package in &closure {
        let ebin = searched.join(package).join("ebin");
        if !ebin.is_dir() {
            return Err(PackagingError::ProjectNotBuilt { missing: ebin });
        }
        collect_package_modules(package, &ebin, &mut provenance, &mut modules, &mut excluded)?;
    }

    Ok(DiscoveredModules {
        modules,
        excluded,
        searched,
    })
}

/// Collects all first-party `src/**/*.gleam` sources keyed by logical module
/// name (the path relative to `src/` without the extension).
pub(crate) fn discover_sources(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, PackagingError> {
    let src_root = root.join("src");
    let mut sources = BTreeMap::new();
    collect_sources(&src_root, "", &mut sources)?;
    Ok(sources)
}

/// Computes the production dependency closure: the root package name plus the
/// transitive closure of `gleam.toml` `[dependencies]` over the lockfile.
fn production_closure(root: &Path) -> Result<BTreeSet<String>, PackagingError> {
    let config_path = root.join(GLEAM_CONFIG_FILE);
    let config_text = match fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(PackagingError::GleamTomlMissing { path: config_path });
        }
        Err(source) => {
            return Err(PackagingError::GleamMetadataRead {
                path: config_path,
                source,
            });
        }
    };
    let config: GleamConfig =
        toml::from_str(&config_text).map_err(|source| PackagingError::GleamMetadataParse {
            path: config_path,
            source,
        })?;

    let lockfile_path = root.join(GLEAM_LOCKFILE);
    let lockfile_text =
        fs::read_to_string(&lockfile_path).map_err(|source| PackagingError::GleamMetadataRead {
            path: lockfile_path.clone(),
            source,
        })?;
    let lockfile: GleamLockfile =
        toml::from_str(&lockfile_text).map_err(|source| PackagingError::GleamMetadataParse {
            path: lockfile_path,
            source,
        })?;

    let requirements: BTreeMap<&str, &[String]> = lockfile
        .packages
        .iter()
        .map(|package| (package.name.as_str(), package.requirements.as_slice()))
        .collect();

    let mut closure = BTreeSet::from([config.name]);
    let mut queue: Vec<String> = config.dependencies.into_keys().collect();
    while let Some(package) = queue.pop() {
        let Some(transitive) = requirements.get(package.as_str()) else {
            return Err(PackagingError::DependencyUnresolved { package });
        };
        if closure.insert(package) {
            queue.extend(transitive.iter().cloned());
        }
    }

    Ok(closure)
}

/// Records every module of built packages outside the production closure.
fn record_dev_dependency_exclusions(
    searched: &Path,
    closure: &BTreeSet<String>,
    excluded: &mut Vec<ExcludedModule>,
) -> Result<(), PackagingError> {
    for package in built_package_names(searched)? {
        if closure.contains(&package) {
            continue;
        }
        let ebin = searched.join(&package).join("ebin");
        if !ebin.is_dir() {
            continue;
        }
        for (module, _) in beam_entries(&ebin)? {
            excluded.push(ExcludedModule {
                module,
                package: package.clone(),
                reason: ExcludedReason::DevDependency,
            });
        }
    }
    Ok(())
}

/// Reads one closure package's ebin, filtering SDK test modules and detecting
/// cross-package duplicates with provenance.
fn collect_package_modules(
    package: &str,
    ebin: &Path,
    provenance: &mut BTreeMap<String, String>,
    modules: &mut Vec<BeamModule>,
    excluded: &mut Vec<ExcludedModule>,
) -> Result<(), PackagingError> {
    for (module, path) in beam_entries(ebin)? {
        if package == SDK_PACKAGE && is_sdk_test_only(&module) {
            excluded.push(ExcludedModule {
                module,
                package: package.to_owned(),
                reason: ExcludedReason::SdkTestOnly,
            });
            continue;
        }
        if let Some(first) = provenance.get(&module) {
            return Err(PackagingError::DuplicateModule {
                module,
                first: first.clone(),
                second: package.to_owned(),
            });
        }
        provenance.insert(module.clone(), package.to_owned());

        let bytes = fs::read(&path).map_err(|source| PackagingError::BeamRead {
            path: path.clone(),
            source,
        })?;
        modules.push(BeamModule::new(module, bytes));
    }
    Ok(())
}

/// Lists the package directory names under `build/dev/erlang`, sorted.
///
/// Non-directory entries and directories with non-UTF-8 names are skipped: they
/// cannot name a Gleam package, so they are neither closure members nor
/// reportable exclusions.
fn built_package_names(searched: &Path) -> Result<Vec<String>, PackagingError> {
    let entries = fs::read_dir(searched).map_err(|source| PackagingError::BeamRead {
        path: searched.to_path_buf(),
        source,
    })?;

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| PackagingError::BeamRead {
            path: searched.to_path_buf(),
            source,
        })?;
        if entry.path().is_dir()
            && let Some(name) = entry.file_name().to_str()
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Lists `(module name, path)` for every `.beam` file in an ebin directory,
/// sorted by module name. Non-`.beam` entries are skipped silently.
fn beam_entries(ebin: &Path) -> Result<Vec<(String, PathBuf)>, PackagingError> {
    let entries = fs::read_dir(ebin).map_err(|source| PackagingError::BeamRead {
        path: ebin.to_path_buf(),
        source,
    })?;

    let mut beams = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| PackagingError::BeamRead {
            path: ebin.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !path.is_file() || path.extension() != Some("beam".as_ref()) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            return Err(PackagingError::ModuleNameNotUtf8 { path });
        };
        beams.push((stem.to_owned(), path.clone()));
    }
    beams.sort();
    Ok(beams)
}

/// SDK test machinery that must never ship inside a workflow package.
///
/// `aion_flow_ffi` is the SDK's in-process engine double occupying the
/// engine-owned NIF namespace, and the `aion/testing` modules exist only to
/// drive it from SDK unit tests. The filter applies solely to the `aion_flow`
/// package's ebin; a user module with one of these names flows through to
/// `BeamSet::new`, where the reserved-name contract rejects it typed.
fn is_sdk_test_only(module: &str) -> bool {
    module == "aion_flow_ffi" || module == "aion@testing" || module.starts_with("aion@testing@")
}

fn collect_sources(
    dir: &Path,
    prefix: &str,
    sources: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), PackagingError> {
    let entries = fs::read_dir(dir).map_err(|source| PackagingError::SourceRead {
        path: dir.to_path_buf(),
        source,
    })?;

    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| PackagingError::SourceRead {
            path: dir.to_path_buf(),
            source,
        })?;
        paths.push(entry.path());
    }
    paths.sort();

    for path in paths {
        if path.is_dir() {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                return Err(PackagingError::ModuleNameNotUtf8 { path });
            };
            let nested_prefix = format!("{prefix}{name}/");
            collect_sources(&path, &nested_prefix, sources)?;
        } else if path.extension() == Some("gleam".as_ref()) {
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                return Err(PackagingError::ModuleNameNotUtf8 { path });
            };
            let logical = format!("{prefix}{stem}");
            let bytes = fs::read(&path).map_err(|source| PackagingError::SourceRead {
                path: path.clone(),
                source,
            })?;
            sources.insert(logical, bytes);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{discover_modules, discover_sources};
    use crate::{
        BeamModule,
        project::{assemble::ExcludedReason, error::PackagingError, fixture},
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn discovers_production_closure_modules_with_exact_bytes() -> TestResult {
        let root = fixture::synthetic_built_project("discover-closure")?;
        let discovered = discover_modules(&root);
        fs::remove_dir_all(&root)?;
        let discovered = discovered?;

        let names: Vec<&str> = discovered.modules.iter().map(BeamModule::name).collect();
        assert_eq!(
            names,
            vec!["aion_flow", "demo", "demo@nested", "dep_a", "dep_b"]
        );
        let demo = discovered
            .modules
            .iter()
            .find(|module| module.name() == "demo")
            .ok_or("demo module missing")?;
        assert_eq!(demo.bytes(), b"demo-bytes");
        assert_eq!(discovered.searched, root.join("build/dev/erlang"));
        Ok(())
    }

    #[test]
    fn dev_dependency_modules_are_excluded_and_reported() -> TestResult {
        let root = fixture::synthetic_built_project("discover-dev-dep")?;
        let discovered = discover_modules(&root);
        fs::remove_dir_all(&root)?;
        let discovered = discovered?;

        assert!(
            discovered
                .modules
                .iter()
                .all(|module| module.name() != "dev_only")
        );
        let dev_exclusions: Vec<_> = discovered
            .excluded
            .iter()
            .filter(|excluded| excluded.reason == ExcludedReason::DevDependency)
            .collect();
        assert_eq!(dev_exclusions.len(), 1);
        assert_eq!(dev_exclusions[0].module, "dev_only");
        assert_eq!(dev_exclusions[0].package, "dev_only");
        Ok(())
    }

    #[test]
    fn sdk_test_modules_are_filtered_only_from_aion_flow_and_reported() -> TestResult {
        let root = fixture::synthetic_built_project("discover-sdk-filter")?;
        let discovered = discover_modules(&root);
        fs::remove_dir_all(&root)?;
        let discovered = discovered?;

        for filtered in ["aion_flow_ffi", "aion@testing", "aion@testing@mock"] {
            assert!(
                discovered
                    .modules
                    .iter()
                    .all(|module| module.name() != filtered),
                "{filtered} should have been filtered"
            );
            assert!(
                discovered.excluded.iter().any(|excluded| {
                    excluded.module == filtered
                        && excluded.package == "aion_flow"
                        && excluded.reason == ExcludedReason::SdkTestOnly
                }),
                "{filtered} exclusion was not reported"
            );
        }
        Ok(())
    }

    #[test]
    fn user_module_with_reserved_name_is_discovered_not_filtered() -> TestResult {
        let root = fixture::synthetic_built_project("discover-user-reserved")?;
        fixture::write_file(
            &root,
            "build/dev/erlang/demo/ebin/aion_flow_ffi.beam",
            b"user-owned-bytes",
        )?;
        let discovered = discover_modules(&root);
        fs::remove_dir_all(&root)?;
        let discovered = discovered?;

        assert!(discovered.modules.iter().any(
            |module| module.name() == "aion_flow_ffi" && module.bytes() == b"user-owned-bytes"
        ));
        Ok(())
    }

    #[test]
    fn missing_build_directory_returns_project_not_built() -> TestResult {
        let root = fixture::synthetic_built_project("discover-unbuilt")?;
        fs::remove_dir_all(root.join("build"))?;
        let result = discover_modules(&root);
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::ProjectNotBuilt { missing })
                if missing == root.join("build/dev/erlang")
        ));
        Ok(())
    }

    #[test]
    fn missing_closure_package_ebin_returns_project_not_built() -> TestResult {
        let root = fixture::synthetic_built_project("discover-missing-ebin")?;
        fs::remove_dir_all(root.join("build/dev/erlang/dep_b"))?;
        let result = discover_modules(&root);
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::ProjectNotBuilt { missing })
                if missing == root.join("build/dev/erlang/dep_b/ebin")
        ));
        Ok(())
    }

    #[test]
    fn gleam_toml_dependency_missing_from_lockfile_is_unresolved() -> TestResult {
        let root = fixture::synthetic_built_project("discover-unresolved-direct")?;
        let lockfile = "packages = [\n  \
             { name = \"aion_flow\", version = \"0.1.0\", requirements = [] },\n\
             ]\n";
        fixture::write_file(&root, "manifest.toml", lockfile.as_bytes())?;
        let result = discover_modules(&root);
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::DependencyUnresolved { package }) if package == "dep_a"
        ));
        Ok(())
    }

    #[test]
    fn transitive_requirement_missing_from_lockfile_is_unresolved() -> TestResult {
        let root = fixture::synthetic_built_project("discover-unresolved-transitive")?;
        let lockfile = "packages = [\n  \
             { name = \"aion_flow\", version = \"0.1.0\", requirements = [] },\n  \
             { name = \"dep_a\", version = \"1.0.0\", requirements = [\"dep_b\"] },\n\
             ]\n";
        fixture::write_file(&root, "manifest.toml", lockfile.as_bytes())?;
        let result = discover_modules(&root);
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::DependencyUnresolved { package }) if package == "dep_b"
        ));
        Ok(())
    }

    #[test]
    fn missing_gleam_toml_returns_gleam_toml_missing() -> TestResult {
        let root = fixture::synthetic_built_project("discover-no-gleam-toml")?;
        fs::remove_file(root.join("gleam.toml"))?;
        let result = discover_modules(&root);
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::GleamTomlMissing { path }) if path == root.join("gleam.toml")
        ));
        Ok(())
    }

    #[test]
    fn missing_lockfile_returns_gleam_metadata_read() -> TestResult {
        let root = fixture::synthetic_built_project("discover-no-lockfile")?;
        fs::remove_file(root.join("manifest.toml"))?;
        let result = discover_modules(&root);
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::GleamMetadataRead { path, .. })
                if path == root.join("manifest.toml")
        ));
        Ok(())
    }

    #[test]
    fn unparseable_gleam_metadata_returns_gleam_metadata_parse() -> TestResult {
        for (case, file) in [("config", "gleam.toml"), ("lockfile", "manifest.toml")] {
            let label = format!("discover-bad-{case}");
            let root = fixture::synthetic_built_project(&label)?;
            fixture::write_file(&root, file, b"not = = toml")?;
            let result = discover_modules(&root);
            fs::remove_dir_all(&root)?;

            assert!(
                matches!(
                    result,
                    Err(PackagingError::GleamMetadataParse { ref path, .. })
                        if *path == root.join(file)
                ),
                "unparseable {file} was not rejected: {result:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn cross_package_duplicate_module_carries_both_provenances() -> TestResult {
        let root = fixture::synthetic_built_project("discover-duplicate")?;
        fixture::write_file(
            &root,
            "build/dev/erlang/dep_b/ebin/dep_a.beam",
            b"impostor-bytes",
        )?;
        let result = discover_modules(&root);
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::DuplicateModule { module, first, second })
                if module == "dep_a" && first == "dep_a" && second == "dep_b"
        ));
        Ok(())
    }

    #[test]
    fn sources_are_collected_recursively_with_logical_names() -> TestResult {
        let root = fixture::synthetic_built_project("discover-sources")?;
        let sources = discover_sources(&root);
        fs::remove_dir_all(&root)?;
        let sources = sources?;

        let names: Vec<&str> = sources.keys().map(String::as_str).collect();
        assert_eq!(names, vec!["demo", "demo/nested"]);
        assert_eq!(
            sources.get("demo").map(Vec::as_slice),
            Some(b"pub fn run() { Nil }".as_slice())
        );
        assert_eq!(
            sources.get("demo/nested").map(Vec::as_slice),
            Some(b"pub fn helper() { Nil }".as_slice())
        );
        Ok(())
    }

    #[test]
    fn missing_src_directory_returns_source_read() -> TestResult {
        let root = fixture::synthetic_built_project("discover-no-src")?;
        fs::remove_dir_all(root.join("src"))?;
        let result = discover_sources(&root);
        fs::remove_dir_all(&root)?;

        assert!(matches!(
            result,
            Err(PackagingError::SourceRead { path, .. }) if path == root.join("src")
        ));
        Ok(())
    }
}
