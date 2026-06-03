use std::{collections::BTreeMap, io::Cursor, time::Duration};

use aion_package::{
    BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
    PackageBuilder, PackageError,
};
use serde_json::json;
use zip::ZipArchive;

fn sample_manifest() -> Manifest {
    Manifest {
        entry_module: "workflow/order".to_owned(),
        entry_function: "run".to_owned(),
        input_schema: json!({ "type": "object" }),
        output_schema: json!({ "type": "object" }),
        timeout: Duration::from_secs(30),
        activities: vec![DeclaredActivity {
            activity_type: "charge_card".to_owned(),
        }],
        version: ManifestVersion::new("placeholder"),
        format_version: CURRENT_FORMAT_VERSION,
    }
}

fn sample_beams() -> Result<BeamSet, PackageError> {
    BeamSet::new(vec![
        BeamModule::new("workflow/support", vec![4, 5, 6]),
        BeamModule::new("workflow/order", vec![1, 2, 3]),
    ])
}

fn source_files() -> BTreeMap<String, Vec<u8>> {
    let mut source = BTreeMap::new();
    source.insert(
        "workflow/order".to_owned(),
        b"pub fn run() { Nil }".to_vec(),
    );
    source
}

#[test]
fn write_to_bytes_produces_fixed_zip_layout() -> Result<(), PackageError> {
    let bytes = PackageBuilder::with_source(sample_manifest(), sample_beams()?, source_files())
        .write_to_bytes()?;
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(PackageError::ArchiveRead)?;
    let mut names = Vec::new();

    for index in 0..archive.len() {
        let file = archive.by_index(index).map_err(PackageError::ArchiveRead)?;
        names.push(file.name().to_owned());
    }

    assert_eq!(
        names,
        vec![
            "manifest.json",
            "beam/workflow/order.beam",
            "beam/workflow/support.beam",
            "src/workflow/order.gleam",
        ]
    );
    Ok(())
}

#[test]
fn identical_inputs_produce_byte_identical_archives() -> Result<(), PackageError> {
    let first = PackageBuilder::with_source(sample_manifest(), sample_beams()?, source_files())
        .write_to_bytes()?;
    let second = PackageBuilder::with_source(sample_manifest(), sample_beams()?, source_files())
        .write_to_bytes()?;

    assert_eq!(first, second);
    Ok(())
}

#[test]
fn source_inclusion_does_not_change_manifest_version() -> Result<(), PackageError> {
    let without_source = PackageBuilder::new(sample_manifest(), sample_beams()?)
        .finalise_manifest()?
        .version;
    let with_source =
        PackageBuilder::with_source(sample_manifest(), sample_beams()?, source_files())
            .finalise_manifest()?
            .version;

    assert_eq!(without_source, with_source);
    Ok(())
}
