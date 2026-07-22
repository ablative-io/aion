//! Integration tests for loading validated `.aion` packages.

use std::{collections::BTreeMap, io::Cursor, time::Duration};

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
    ManifestVersion, Package, PackageBuilder, PackageError, content_hash, deployed_name,
};
use serde_json::json;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

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

fn source_files() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([(
        "workflow/order".to_owned(),
        b"pub fn run() { Nil }".to_vec(),
    )])
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
fn builder_produced_package_loads_successfully() -> Result<(), PackageError> {
    let bytes = PackageBuilder::with_source(sample_manifest(), sample_beams()?, source_files())
        .write_to_bytes()?;

    let package = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

    assert_eq!(package.manifest().entry_module, "workflow/order");
    assert_eq!(package.beams().len(), 2);
    assert_eq!(
        package.content_hash().to_string(),
        package.manifest().version.as_str()
    );
    assert_eq!(
        package.source().get("workflow/order"),
        Some(&b"pub fn run() { Nil }".to_vec())
    );
    Ok(())
}

#[test]
fn corrupted_beam_contents_return_integrity_mismatch() -> Result<(), PackageError> {
    let original = PackageBuilder::new(sample_manifest(), sample_beams()?).write_to_bytes()?;
    let corrupted = rewrite_with_corrupted_order_beam(&original)?;
    let result = Package::load_from_bytes(corrupted, ExtractionLimits::unbounded());

    assert!(matches!(
        result,
        Err(PackageError::IntegrityMismatch { expected, computed })
            if expected != computed
    ));
    Ok(())
}

#[test]
fn deployed_modules_match_namespace_transform_for_every_module() -> Result<(), PackageError> {
    let bytes = PackageBuilder::with_source(sample_manifest(), sample_beams()?, source_files())
        .write_to_bytes()?;
    let package = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

    let deployed = package.deployed_modules();
    let expected: Vec<(String, &[u8])> = package
        .beams()
        .iter()
        .map(|module| {
            (
                deployed_name(module.name(), package.content_hash()),
                module.bytes(),
            )
        })
        .collect();

    assert_eq!(deployed, expected);
    assert_eq!(
        package.deployed_entry_module(),
        deployed_name("workflow/order", package.content_hash())
    );
    Ok(())
}

#[test]
fn loaded_hash_matches_canonical_beam_hash() -> Result<(), PackageError> {
    let bytes = PackageBuilder::new(sample_manifest(), sample_beams()?).write_to_bytes()?;
    let package = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

    assert_eq!(package.content_hash(), &content_hash(package.beams()));
    Ok(())
}
