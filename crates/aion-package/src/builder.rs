//! `PackageBuilder` deterministic write path.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{Cursor, Seek, Write},
    path::Path,
};

use zip::{CompressionMethod, DateTime, ZipWriter, write::SimpleFileOptions};

use crate::{BeamSet, Manifest, ManifestVersion, PackageError, content_hash};

/// Deterministic writer for the `.aion` ZIP container format.
#[derive(Clone, Debug)]
pub struct PackageBuilder {
    manifest: Manifest,
    beams: BeamSet,
    source: BTreeMap<String, Vec<u8>>,
}

impl PackageBuilder {
    /// Creates a builder without source files.
    #[must_use]
    pub fn new(manifest: Manifest, beams: BeamSet) -> Self {
        Self {
            manifest,
            beams,
            source: BTreeMap::new(),
        }
    }

    /// Creates a builder with optional source files keyed by logical module name.
    #[must_use]
    pub fn with_source<I, N, B>(manifest: Manifest, beams: BeamSet, source: I) -> Self
    where
        I: IntoIterator<Item = (N, B)>,
        N: Into<String>,
        B: Into<Vec<u8>>,
    {
        Self {
            manifest,
            beams,
            source: source
                .into_iter()
                .map(|(name, bytes)| (name.into(), bytes.into()))
                .collect(),
        }
    }

    /// Returns the manifest after stamping the authoritative beam content hash.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::MissingEntryModule`] when the manifest entry module
    /// is not present in the supplied beam set.
    pub fn finalise_manifest(&self) -> Result<Manifest, PackageError> {
        self.stamped_manifest()
    }

    /// Writes a deterministic `.aion` archive into memory.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError`] variants for missing entry modules, manifest JSON
    /// serialisation failures, ZIP writer failures, or target I/O failures.
    pub fn write_to_bytes(&self) -> Result<Vec<u8>, PackageError> {
        let cursor = Cursor::new(Vec::new());
        let manifest_bytes = self.manifest_bytes()?;
        let cursor = self.write_archive(cursor, &manifest_bytes)?;
        Ok(cursor.into_inner())
    }

    /// Writes a deterministic `.aion` archive to the supplied filesystem path.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError`] variants for missing entry modules, manifest JSON
    /// serialisation failures, ZIP writer failures, or target I/O failures.
    pub fn write_to_path(&self, path: impl AsRef<Path>) -> Result<(), PackageError> {
        let manifest_bytes = self.manifest_bytes()?;
        let file = File::create(path).map_err(|source| PackageError::ArchiveWriteIo { source })?;
        self.write_archive(file, &manifest_bytes)?;
        Ok(())
    }

    fn manifest_bytes(&self) -> Result<Vec<u8>, PackageError> {
        let manifest = self.stamped_manifest()?;
        serde_json::to_vec(&manifest).map_err(|source| PackageError::ManifestSerialise { source })
    }

    fn stamped_manifest(&self) -> Result<Manifest, PackageError> {
        if self.beams.get(&self.manifest.entry_module).is_none() {
            return Err(PackageError::MissingEntryModule {
                module: self.manifest.entry_module.clone(),
            });
        }

        let hash = content_hash(&self.beams);
        let mut manifest = self.manifest.clone();
        manifest.version = ManifestVersion::new(hash.to_string());
        Ok(manifest)
    }

    fn write_archive<W>(&self, writer: W, manifest_bytes: &[u8]) -> Result<W, PackageError>
    where
        W: Write + Seek,
    {
        let mut archive = ZipWriter::new(writer);
        let options = deterministic_file_options();

        write_entry(&mut archive, "manifest.json", manifest_bytes, options)?;

        for module in self.beams.iter() {
            let entry_name = archive_entry_name("beam", module.name(), "beam")?;
            write_entry(&mut archive, entry_name, module.bytes(), options)?;
        }

        for (name, bytes) in &self.source {
            let entry_name = archive_entry_name("src", name, "gleam")?;
            write_entry(&mut archive, entry_name, bytes, options)?;
        }

        archive.finish().map_err(PackageError::ArchiveWrite)
    }
}

fn deterministic_file_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .compression_level(None)
        .last_modified_time(DateTime::DEFAULT)
        .unix_permissions(0o644)
}

fn archive_entry_name(
    prefix: &str,
    logical_name: &str,
    extension: &str,
) -> Result<String, PackageError> {
    if is_safe_logical_name(logical_name) {
        Ok(format!("{prefix}/{logical_name}.{extension}"))
    } else {
        Err(PackageError::MalformedBeamEntry {
            entry: logical_name.to_owned(),
        })
    }
}

pub(crate) fn is_safe_logical_name(logical_name: &str) -> bool {
    !logical_name.is_empty()
        && !logical_name.starts_with('/')
        && !logical_name.starts_with('\\')
        && !logical_name.contains('\\')
        && !logical_name.contains(crate::namespace::DEPLOYED_NAME_SEPARATOR)
        && logical_name
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn write_entry<W>(
    archive: &mut ZipWriter<W>,
    name: impl ToString,
    bytes: &[u8],
    options: SimpleFileOptions,
) -> Result<(), PackageError>
where
    W: Write + Seek,
{
    archive
        .start_file(name, options)
        .map_err(PackageError::ArchiveWrite)?;
    archive
        .write_all(bytes)
        .map_err(|source| PackageError::ArchiveWriteIo { source })
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, io::Cursor, time::Duration};

    use serde_json::json;
    use zip::ZipArchive;

    use super::PackageBuilder;
    use crate::{
        BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion,
        PackageError, content_hash,
    };

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
            version: ManifestVersion::new("caller-supplied-version"),
            format_version: CURRENT_FORMAT_VERSION,
        }
    }

    fn sample_beams() -> Result<BeamSet, PackageError> {
        BeamSet::new(vec![
            BeamModule::new("workflow/support", vec![4, 5, 6]),
            BeamModule::new("workflow/order", vec![1, 2, 3]),
        ])
    }

    #[test]
    fn finalised_manifest_version_equals_beam_content_hash() -> Result<(), PackageError> {
        let beams = sample_beams()?;
        let expected = content_hash(&beams).to_string();
        let manifest = PackageBuilder::new(sample_manifest(), beams).finalise_manifest()?;

        assert_eq!(manifest.version.as_str(), expected);
        Ok(())
    }

    #[test]
    fn caller_supplied_manifest_version_is_overwritten() -> Result<(), PackageError> {
        let beams = sample_beams()?;
        let expected = content_hash(&beams).to_string();
        let manifest = PackageBuilder::new(sample_manifest(), beams).finalise_manifest()?;

        assert_ne!(manifest.version.as_str(), "caller-supplied-version");
        assert_eq!(manifest.version.as_str(), expected);
        Ok(())
    }

    #[test]
    fn missing_entry_module_returns_typed_error() -> Result<(), PackageError> {
        let beams = BeamSet::new(vec![BeamModule::new("workflow/other", vec![1])])?;
        let result = PackageBuilder::new(sample_manifest(), beams).write_to_bytes();

        assert!(matches!(
            result,
            Err(PackageError::MissingEntryModule { module }) if module == "workflow/order"
        ));
        Ok(())
    }

    #[test]
    fn write_to_bytes_succeeds_without_source_entries() -> Result<(), PackageError> {
        let bytes = PackageBuilder::new(sample_manifest(), sample_beams()?).write_to_bytes()?;
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
            ]
        );
        Ok(())
    }

    #[test]
    fn identical_inputs_produce_identical_archive_bytes() -> Result<(), PackageError> {
        let mut source = BTreeMap::new();
        source.insert(
            "workflow/order".to_owned(),
            b"pub fn run() { Nil }".to_vec(),
        );
        let first = PackageBuilder::with_source(sample_manifest(), sample_beams()?, source.clone())
            .write_to_bytes()?;
        let second = PackageBuilder::with_source(sample_manifest(), sample_beams()?, source)
            .write_to_bytes()?;

        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn source_inclusion_does_not_change_manifest_version() -> Result<(), PackageError> {
        let mut source = BTreeMap::new();
        source.insert(
            "workflow/order".to_owned(),
            b"pub fn run() { Nil }".to_vec(),
        );
        let without_source = PackageBuilder::new(sample_manifest(), sample_beams()?)
            .finalise_manifest()?
            .version;
        let with_source = PackageBuilder::with_source(sample_manifest(), sample_beams()?, source)
            .finalise_manifest()?
            .version;

        assert_eq!(without_source, with_source);
        Ok(())
    }

    #[test]
    fn rejects_unsafe_source_names() -> Result<(), PackageError> {
        let mut source = BTreeMap::new();
        source.insert("../escape".to_owned(), b"pub fn run() { Nil }".to_vec());

        let result = PackageBuilder::with_source(sample_manifest(), sample_beams()?, source)
            .write_to_bytes();

        assert!(matches!(
            result,
            Err(PackageError::MalformedBeamEntry { entry }) if entry == "../escape"
        ));
        Ok(())
    }

    #[test]
    fn rejects_logical_names_with_deployed_name_separator() -> Result<(), PackageError> {
        let beams = BeamSet::new(vec![
            BeamModule::new("workflow/order", vec![1, 2, 3]),
            BeamModule::new("workflow/order$bad", vec![1]),
        ])?;
        let result = PackageBuilder::new(sample_manifest(), beams).write_to_bytes();

        assert!(matches!(
            result,
            Err(PackageError::MalformedBeamEntry { entry }) if entry == "workflow/order$bad"
        ));
        Ok(())
    }
}
