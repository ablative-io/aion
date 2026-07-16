//! Round-trip conformance tests for `.aion` package production and loading.

use std::{collections::BTreeMap, io::Cursor, time::Duration};

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder, PackageError, deployed_name,
};
use serde_json::json;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

fn conformance_manifest() -> Manifest {
    Manifest {
        entry_module: "workflow/order".to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["order_id", "line_items"],
            "properties": {
                "order_id": { "type": "string", "minLength": 1 },
                "line_items": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["sku", "quantity"],
                        "properties": {
                            "sku": { "type": "string" },
                            "quantity": { "type": "integer", "minimum": 1 }
                        }
                    }
                }
            }
        }),
        output_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["status", "confirmation_id"],
            "properties": {
                "status": { "enum": ["accepted", "rejected"] },
                "confirmation_id": { "type": "string" },
                "total_cents": { "type": "integer" }
            }
        }),
        timeout: Duration::from_secs(45),
        activities: vec![
            DeclaredActivity {
                activity_type: "reserve_inventory".to_owned(),
            },
            DeclaredActivity {
                activity_type: "charge_card".to_owned(),
            },
        ],
        version: ManifestVersion::new("placeholder"),
        format_version: CURRENT_FORMAT_VERSION,
        additional_workflows: Vec::new(),
    }
}

fn conformance_beams() -> Result<BeamSet, PackageError> {
    BeamSet::new(vec![
        BeamModule::new("workflow/support/validation", vec![10, 20, 30, 40]),
        BeamModule::new("workflow/order", vec![1, 2, 3, 5, 8, 13]),
        BeamModule::new("workflow/support/pricing", vec![21, 34, 55]),
    ])
}

fn source_files() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        (
            "workflow/order".to_owned(),
            b"pub fn run(input) { input }".to_vec(),
        ),
        (
            "workflow/support/pricing".to_owned(),
            b"pub fn calculate_total(items) { items }".to_vec(),
        ),
    ])
}

fn rewrite_with_corrupted_order_beam(bytes: &[u8]) -> Result<Vec<u8>, PackageError> {
    let mut input = ZipArchive::new(Cursor::new(bytes)).map_err(PackageError::ArchiveRead)?;
    let cursor = Cursor::new(Vec::new());
    let mut output = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .compression_level(None);

    for index in 0..input.len() {
        let mut file = input.by_index(index).map_err(PackageError::ArchiveRead)?;
        if file.is_dir() {
            continue;
        }

        let name = file.name().to_owned();
        let mut entry_bytes = Vec::new();
        std::io::copy(&mut file, &mut entry_bytes)
            .map_err(|source| PackageError::ArchiveRead(zip::result::ZipError::Io(source)))?;
        if name == "beam/workflow/order.beam" {
            entry_bytes.push(99);
        }

        output
            .start_file(name, options)
            .map_err(PackageError::ArchiveWrite)?;
        std::io::copy(&mut Cursor::new(entry_bytes), &mut output)
            .map_err(|source| PackageError::ArchiveWriteIo { source })?;
    }

    let cursor = output.finish().map_err(PackageError::ArchiveWrite)?;
    Ok(cursor.into_inner())
}

#[test]
fn package_round_trips_manifest_beams_source_and_deployed_names() -> Result<(), PackageError> {
    let manifest = conformance_manifest();
    let beams = conformance_beams()?;
    let source = source_files();
    let builder = PackageBuilder::with_source(manifest, beams.clone(), source.clone());
    let expected_manifest = builder.finalise_manifest()?;
    let bytes = builder.write_to_bytes()?;

    let loaded = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

    assert_eq!(loaded.manifest(), &expected_manifest);
    assert_eq!(loaded.beams(), &beams);
    assert_eq!(loaded.source(), &source);

    let expected_deployed: Vec<(String, &[u8])> = loaded
        .beams()
        .iter()
        .map(|module| {
            (
                deployed_name(module.name(), loaded.content_hash()),
                module.bytes(),
            )
        })
        .collect();
    assert_eq!(loaded.deployed_modules(), expected_deployed);
    assert_eq!(
        loaded.deployed_entry_module(),
        deployed_name(&expected_manifest.entry_module, loaded.content_hash())
    );
    Ok(())
}

#[test]
fn package_round_trips_without_source() -> Result<(), PackageError> {
    let beams = conformance_beams()?;
    let builder = PackageBuilder::new(conformance_manifest(), beams.clone());
    let expected_manifest = builder.finalise_manifest()?;
    let bytes = builder.write_to_bytes()?;

    let loaded = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

    assert_eq!(loaded.manifest(), &expected_manifest);
    assert_eq!(loaded.beams(), &beams);
    assert!(loaded.source().is_empty());
    Ok(())
}

#[test]
fn identical_inputs_produce_byte_identical_packages() -> Result<(), PackageError> {
    let first =
        PackageBuilder::with_source(conformance_manifest(), conformance_beams()?, source_files())
            .write_to_bytes()?;
    let second =
        PackageBuilder::with_source(conformance_manifest(), conformance_beams()?, source_files())
            .write_to_bytes()?;

    assert_eq!(first, second);
    Ok(())
}

#[test]
fn altering_packed_beam_contents_is_rejected_as_integrity_mismatch() -> Result<(), PackageError> {
    let original =
        PackageBuilder::with_source(conformance_manifest(), conformance_beams()?, source_files())
            .write_to_bytes()?;
    let corrupted = rewrite_with_corrupted_order_beam(&original)?;

    let result = Package::load_from_bytes(corrupted, ExtractionLimits::unbounded());

    assert!(matches!(
        result,
        Err(PackageError::IntegrityMismatch { expected, computed }) if expected != computed
    ));
    Ok(())
}

#[test]
fn source_inclusion_does_not_change_loaded_content_hash_or_manifest_version()
-> Result<(), PackageError> {
    let without_source =
        PackageBuilder::new(conformance_manifest(), conformance_beams()?).write_to_bytes()?;
    let with_source =
        PackageBuilder::with_source(conformance_manifest(), conformance_beams()?, source_files())
            .write_to_bytes()?;

    let loaded_without_source =
        Package::load_from_bytes(without_source, ExtractionLimits::unbounded())?;
    let loaded_with_source = Package::load_from_bytes(with_source, ExtractionLimits::unbounded())?;

    assert_eq!(
        loaded_without_source.content_hash(),
        loaded_with_source.content_hash()
    );
    assert_eq!(
        loaded_without_source.manifest().version,
        loaded_with_source.manifest().version
    );
    assert!(loaded_without_source.source().is_empty());
    assert_eq!(loaded_with_source.source(), &source_files());
    Ok(())
}
