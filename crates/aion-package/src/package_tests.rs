use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Write},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::json;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use super::Package;
use crate::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, PackageBuilder, PackageError, content_hash,
};

fn sample_manifest() -> Manifest {
    Manifest {
        entry_module: "workflow/order".to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "object" }),
        timeout: Some(Duration::from_secs(30)),
        activities: vec![DeclaredActivity {
            activity_type: "charge_card".to_owned(),
        }],
        version: ManifestVersion::new("placeholder"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    }
}

fn sample_beams() -> Result<BeamSet, PackageError> {
    BeamSet::new(vec![
        BeamModule::new("workflow/support", vec![4, 5, 6]),
        BeamModule::new("workflow/order", vec![1, 2, 3]),
    ])
}

fn write_zip<I, N, B>(entries: I) -> Result<Vec<u8>, PackageError>
where
    I: IntoIterator<Item = (N, B)>,
    N: ToString,
    B: AsRef<[u8]>,
{
    let cursor = Cursor::new(Vec::new());
    let mut archive = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .compression_level(None);

    for (name, bytes) in entries {
        archive
            .start_file(name, options)
            .map_err(PackageError::ArchiveWrite)?;
        archive
            .write_all(bytes.as_ref())
            .map_err(|source| PackageError::ArchiveWriteIo { source })?;
    }

    let cursor = archive.finish().map_err(PackageError::ArchiveWrite)?;
    Ok(cursor.into_inner())
}

fn archive_with_manifest(manifest: &Manifest) -> Result<Vec<u8>, PackageError> {
    let manifest_bytes = serde_json::to_vec(manifest)
        .map_err(|source| PackageError::ManifestSerialise { source })?;
    write_zip([("manifest.json", manifest_bytes)])
}

/// Writes a `.aion` archive carrying `manifest` plus every module in `beams`,
/// so a hand-crafted (possibly tampered) manifest can be loaded through the real
/// [`Package::load_from_bytes`] integrity path.
fn archive_with_beams(manifest: &Manifest, beams: &BeamSet) -> Result<Vec<u8>, PackageError> {
    let manifest_bytes = serde_json::to_vec(manifest)
        .map_err(|source| PackageError::ManifestSerialise { source })?;
    let mut entries: Vec<(String, Vec<u8>)> = vec![("manifest.json".to_owned(), manifest_bytes)];
    for module in beams.iter() {
        entries.push((
            format!("beam/{}.beam", module.name()),
            module.bytes().to_vec(),
        ));
    }
    write_zip(entries)
}

/// A DEFLATE-compressed zip — what a hostile uploader sends.
/// [`crate::PackageBuilder`] writes Stored entries only, so bombs must be
/// assembled here.
fn deflated_zip<I, N, B>(entries: I) -> Result<Vec<u8>, PackageError>
where
    I: IntoIterator<Item = (N, B)>,
    N: ToString,
    B: AsRef<[u8]>,
{
    let cursor = Cursor::new(Vec::new());
    let mut archive = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    for (name, bytes) in entries {
        archive
            .start_file(name, options)
            .map_err(PackageError::ArchiveWrite)?;
        archive
            .write_all(bytes.as_ref())
            .map_err(|source| PackageError::ArchiveWriteIo { source })?;
    }

    let cursor = archive.finish().map_err(PackageError::ArchiveWrite)?;
    Ok(cursor.into_inner())
}

/// A compressed upload far smaller than its inflated contents (a zip
/// bomb) must be refused as soon as the running inflate total would pass
/// the bounded budget — loudly, reporting the configured limit.
#[test]
fn inflate_bomb_past_bounded_budget_is_refused_reporting_the_limit()
-> Result<(), Box<dyn std::error::Error>> {
    const BUDGET: u64 = 65_536;
    let manifest_bytes = serde_json::to_vec(&sample_manifest())
        .map_err(|source| PackageError::ManifestSerialise { source })?;
    let bytes = deflated_zip([
        ("manifest.json", manifest_bytes),
        ("beam/workflow/order.beam", vec![0_u8; 4 * 1024 * 1024]),
    ])?;
    assert!(
        u64::try_from(bytes.len())? < BUDGET,
        "bomb must compress under the budget to model a sneaky upload: {} bytes",
        bytes.len()
    );

    let result = Package::load_from_bytes(&bytes, ExtractionLimits::bounded(BUDGET));

    assert!(matches!(
        result,
        Err(PackageError::InflatedSizeExceeded { limit: BUDGET })
    ));
    Ok(())
}

/// The budget is exact: contents inflating to precisely the budget load,
/// and one byte less refuses — no truncation, no slack.
#[test]
fn package_on_exact_inflate_budget_loads_and_one_byte_under_refuses()
-> Result<(), Box<dyn std::error::Error>> {
    let beams = sample_beams()?;
    let mut manifest = sample_manifest();
    manifest.version = ManifestVersion::new(content_hash(&beams).to_string());
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let entries = vec![
        ("manifest.json".to_owned(), manifest_bytes.clone()),
        ("beam/workflow/support.beam".to_owned(), vec![4, 5, 6]),
        ("beam/workflow/order.beam".to_owned(), vec![1, 2, 3]),
    ];
    let inflated_total = u64::try_from(manifest_bytes.len())? + 6;
    let bytes = write_zip(entries)?;

    let loaded = Package::load_from_bytes(&bytes, ExtractionLimits::bounded(inflated_total))?;
    assert_eq!(loaded.beams().len(), 2);

    let result = Package::load_from_bytes(&bytes, ExtractionLimits::bounded(inflated_total - 1));
    assert!(matches!(
        result,
        Err(PackageError::InflatedSizeExceeded { limit }) if limit == inflated_total - 1
    ));
    Ok(())
}

#[test]
fn non_zip_input_returns_archive_read() {
    let result = Package::load_from_bytes(b"not a zip archive", ExtractionLimits::unbounded());

    assert!(matches!(result, Err(PackageError::ArchiveRead(_))));
}

#[test]
fn truncated_zip_input_returns_archive_read() -> Result<(), PackageError> {
    let bytes = archive_with_manifest(&sample_manifest())?;
    let truncated = &bytes[..bytes.len() / 2];
    let result = Package::load_from_bytes(truncated, ExtractionLimits::unbounded());

    assert!(matches!(result, Err(PackageError::ArchiveRead(_))));
    Ok(())
}

#[test]
fn missing_manifest_returns_missing_manifest() -> Result<(), PackageError> {
    let bytes = write_zip([("beam/workflow/order.beam", vec![1, 2, 3])])?;
    let result = Package::load_from_bytes(bytes, ExtractionLimits::unbounded());

    assert!(matches!(result, Err(PackageError::MissingManifest)));
    Ok(())
}

#[test]
fn unparseable_manifest_returns_manifest_parse() -> Result<(), PackageError> {
    let bytes = write_zip([("manifest.json", b"not-json".to_vec())])?;
    let result = Package::load_from_bytes(bytes, ExtractionLimits::unbounded());

    assert!(matches!(result, Err(PackageError::ManifestParse { .. })));
    Ok(())
}

#[test]
fn unknown_format_version_returns_exact_variant() -> Result<(), PackageError> {
    let mut manifest = sample_manifest();
    manifest.format_version = CURRENT_FORMAT_VERSION + 99;
    let bytes = archive_with_manifest(&manifest)?;
    let result = Package::load_from_bytes(bytes, ExtractionLimits::unbounded());

    assert!(matches!(
        result,
        Err(PackageError::UnknownFormatVersion { found }) if found == CURRENT_FORMAT_VERSION + 99
    ));
    Ok(())
}

#[test]
fn malformed_beam_entry_returns_exact_variant() -> Result<(), PackageError> {
    let beams = sample_beams()?;
    let mut manifest = sample_manifest();
    manifest.version = ManifestVersion::new(content_hash(&beams).to_string());
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|source| PackageError::ManifestSerialise { source })?;
    let bytes = write_zip([
        ("manifest.json", manifest_bytes),
        ("beam/workflow/order.txt", vec![1, 2, 3]),
    ])?;
    let result = Package::load_from_bytes(bytes, ExtractionLimits::unbounded());

    assert!(matches!(
        result,
        Err(PackageError::MalformedBeamEntry { entry }) if entry == "beam/workflow/order.txt"
    ));
    Ok(())
}

#[test]
fn beam_entry_with_deployed_name_separator_returns_malformed_entry() -> Result<(), PackageError> {
    let beams = sample_beams()?;
    let mut manifest = sample_manifest();
    manifest.version = ManifestVersion::new(content_hash(&beams).to_string());
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|source| PackageError::ManifestSerialise { source })?;
    let bytes = write_zip([
        ("manifest.json", manifest_bytes),
        ("beam/workflow/order$bad.beam", vec![1, 2, 3]),
    ])?;
    let result = Package::load_from_bytes(bytes, ExtractionLimits::unbounded());

    assert!(matches!(
        result,
        Err(PackageError::MalformedBeamEntry { entry }) if entry == "beam/workflow/order$bad.beam"
    ));
    Ok(())
}

#[test]
fn invalid_source_entry_returns_malformed_entry() -> Result<(), PackageError> {
    let beams = sample_beams()?;
    let mut manifest = sample_manifest();
    manifest.version = ManifestVersion::new(content_hash(&beams).to_string());
    let mut entries = vec![
        (
            "manifest.json".to_owned(),
            serde_json::to_vec(&manifest)
                .map_err(|source| PackageError::ManifestSerialise { source })?,
        ),
        ("src/workflow/order.txt".to_owned(), b"source".to_vec()),
    ];
    entries.extend(beams.iter().map(|module| {
        (
            format!("beam/{}.beam", module.name()),
            module.bytes().to_vec(),
        )
    }));
    let result = Package::load_from_bytes(write_zip(entries)?, ExtractionLimits::unbounded());

    assert!(matches!(
        result,
        Err(PackageError::MalformedBeamEntry { entry }) if entry == "src/workflow/order.txt"
    ));
    Ok(())
}

#[test]
fn missing_entry_module_returns_exact_variant_when_hash_matches() -> Result<(), PackageError> {
    let beams = BeamSet::new(vec![BeamModule::new("workflow/support", vec![4, 5, 6])])?;
    let mut manifest = sample_manifest();
    manifest.version = ManifestVersion::new(content_hash(&beams).to_string());
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|source| PackageError::ManifestSerialise { source })?;
    let bytes = write_zip([
        ("manifest.json", manifest_bytes),
        ("beam/workflow/support.beam", vec![4, 5, 6]),
    ])?;
    let result = Package::load_from_bytes(bytes, ExtractionLimits::unbounded());

    assert!(matches!(
        result,
        Err(PackageError::MissingEntryModule { module }) if module == "workflow/order"
    ));
    Ok(())
}

#[test]
fn builder_produced_package_loads_successfully() -> Result<(), PackageError> {
    let bytes = PackageBuilder::with_source(
        sample_manifest(),
        sample_beams()?,
        BTreeMap::from([(
            "workflow/order".to_owned(),
            b"pub fn run() { Nil }".to_vec(),
        )]),
    )
    .write_to_bytes()?;

    let package = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

    assert_eq!(package.manifest().entry_module, "workflow/order");
    assert_eq!(package.beams().len(), 2);
    assert_eq!(
        package.source().get("workflow/order"),
        Some(&b"pub fn run() { Nil }".to_vec())
    );
    assert_eq!(
        package.content_hash().to_string(),
        package.manifest().version.as_str()
    );
    Ok(())
}

#[test]
fn to_archive_bytes_preserves_legacy_identity() -> Result<(), PackageError> {
    let bytes = PackageBuilder::with_source(
        sample_manifest(),
        sample_beams()?,
        BTreeMap::from([(
            "workflow/order".to_owned(),
            b"pub fn run() { Nil }".to_vec(),
        )]),
    )
    .write_to_bytes()?;
    let package = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

    let reloaded =
        Package::load_from_bytes(package.to_archive_bytes()?, ExtractionLimits::unbounded())?;

    assert_eq!(reloaded, package);
    assert_eq!(
        reloaded.manifest().canonical_digest()?,
        package.manifest().canonical_digest()?
    );
    Ok(())
}

#[test]
fn to_archive_bytes_preserves_explicit_timeout_identity() -> Result<(), PackageError> {
    let mut manifest = sample_manifest();
    manifest.timeout = Some(Duration::new(7_200, 500_000_000));
    let bytes = PackageBuilder::with_source(
        manifest,
        sample_beams()?,
        BTreeMap::from([(
            "workflow/order".to_owned(),
            b"pub fn run() { Nil }".to_vec(),
        )]),
    )
    .with_explicit_timeout_identity()
    .write_to_bytes()?;
    let package = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

    let reloaded =
        Package::load_from_bytes(package.to_archive_bytes()?, ExtractionLimits::unbounded())?;

    assert_eq!(reloaded, package);
    assert_eq!(reloaded.content_hash(), package.content_hash());
    Ok(())
}

#[test]
fn re_routing_the_primary_entry_module_invalidates_the_stored_identity() -> Result<(), PackageError>
{
    // LAW-2 at the load boundary: a package whose stored `.v3` identity
    // authenticated `workflow/order` as the selected primary (with a 30s
    // timeout) cannot be re-pointed to `workflow/support` while keeping that
    // identity. The loader rejects the tampered archive rather than handing the
    // authenticated timeout to a re-routed selected workflow.
    let beams = sample_beams()?;
    let mut manifest = sample_manifest();
    manifest.timeout = Some(Duration::from_secs(30));
    let honest = crate::content_hash_with_timeouts(&beams, &manifest);
    manifest.version = ManifestVersion::new(honest.to_string());
    // Tamper: re-point the primary to another module in the same closure while
    // keeping the stored identity that authenticated `workflow/order`.
    manifest.entry_module = "workflow/support".to_owned();
    let bytes = archive_with_beams(&manifest, &beams)?;

    let result = Package::load_from_bytes(bytes, ExtractionLimits::unbounded());
    assert!(
        matches!(result, Err(PackageError::IntegrityMismatch { .. })),
        "re-routing the primary entry must fail integrity, got {result:?}",
    );
    Ok(())
}

#[test]
fn load_from_path_loads_successfully() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = PackageBuilder::new(sample_manifest(), sample_beams()?).write_to_bytes()?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = std::env::temp_dir().join(format!("aion-package-{nanos}.aion"));
    fs::write(&path, bytes)?;

    let package_result = Package::load_from_path(&path, ExtractionLimits::unbounded());
    let remove_result = fs::remove_file(&path);

    let package = package_result?;
    remove_result?;
    assert_eq!(package.manifest().entry_module, "workflow/order");
    Ok(())
}
