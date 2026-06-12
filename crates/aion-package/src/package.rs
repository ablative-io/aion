//! `Package` load path and integrity check.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{Cursor, Read, Seek},
    path::Path,
};

use zip::{ZipArchive, result::ZipError};

use crate::{
    BeamModule, BeamSet, ContentHash, ExtractionLimits, Manifest, PackageError,
    builder::is_safe_logical_name, content_hash, extraction::ExtractionBudget,
    namespace::deployed_name, version::WorkflowVersion,
};

const MANIFEST_ENTRY: &str = "manifest.json";
const BEAM_PREFIX: &str = "beam/";
const BEAM_SUFFIX: &str = ".beam";
const SOURCE_PREFIX: &str = "src/";
const SOURCE_SUFFIX: &str = ".gleam";

/// A validated, integrity-checked `.aion` package loaded fully into memory.
///
/// The engine performs actual VM registration. This crate only supplies the
/// validated manifest, canonical beam bytes, optional source, and deployed module
/// names the engine can register.
#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    manifest: Manifest,
    beams: BeamSet,
    source: BTreeMap<String, Vec<u8>>,
    content_hash: ContentHash,
}

impl Package {
    /// Loads a `.aion` package from a filesystem path.
    ///
    /// The caller chooses an explicit [`ExtractionLimits`] inflate budget;
    /// untrusted input must be bounded.
    ///
    /// # Errors
    ///
    /// Returns a typed [`PackageError`] for unreadable archives, malformed
    /// manifests or entries, unsupported format versions, integrity mismatches,
    /// missing entry modules, or contents inflating past `limits`.
    pub fn load_from_path(
        path: impl AsRef<Path>,
        limits: ExtractionLimits,
    ) -> Result<Self, PackageError> {
        let file =
            File::open(path).map_err(|source| PackageError::ArchiveRead(ZipError::Io(source)))?;
        Self::load_from_reader(file, limits)
    }

    /// Loads a `.aion` package from an in-memory byte buffer.
    ///
    /// The caller chooses an explicit [`ExtractionLimits`] inflate budget;
    /// untrusted input must be bounded.
    ///
    /// # Errors
    ///
    /// Returns a typed [`PackageError`] for unreadable archives, malformed
    /// manifests or entries, unsupported format versions, integrity mismatches,
    /// missing entry modules, or contents inflating past `limits`.
    pub fn load_from_bytes(
        bytes: impl AsRef<[u8]>,
        limits: ExtractionLimits,
    ) -> Result<Self, PackageError> {
        Self::load_from_reader(Cursor::new(bytes.as_ref()), limits)
    }

    fn load_from_reader<R>(reader: R, limits: ExtractionLimits) -> Result<Self, PackageError>
    where
        R: Read + Seek,
    {
        let mut archive = ZipArchive::new(reader).map_err(PackageError::ArchiveRead)?;
        let mut budget = limits.budget();
        let manifest = read_manifest(&mut archive, &mut budget)?;
        manifest.check_format_version()?;

        let (beams, source) = read_archive_entries(&mut archive, &mut budget)?;
        let content_hash = content_hash(&beams);
        let computed = content_hash.to_string();
        if manifest.version.as_str() != computed {
            return Err(PackageError::IntegrityMismatch {
                expected: manifest.version.as_str().to_owned(),
                computed,
            });
        }

        if beams.get(&manifest.entry_module).is_none() {
            return Err(PackageError::MissingEntryModule {
                module: manifest.entry_module.clone(),
            });
        }

        Ok(Self {
            manifest,
            beams,
            source,
            content_hash,
        })
    }

    /// Returns the validated manifest loaded from `manifest.json`.
    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Returns the canonical compiled beam set extracted from `beam/` entries.
    #[must_use]
    pub const fn beams(&self) -> &BeamSet {
        &self.beams
    }

    /// Returns optional Gleam source files extracted verbatim from `src/` entries.
    #[must_use]
    pub const fn source(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.source
    }

    /// Returns the recomputed content hash that proved package integrity.
    #[must_use]
    pub const fn content_hash(&self) -> &ContentHash {
        &self.content_hash
    }

    /// Produces the canonical cross-system version record for this loaded package.
    #[must_use]
    pub fn version_record(&self) -> WorkflowVersion {
        WorkflowVersion {
            entry_module: self.manifest.entry_module.clone(),
            content_hash: self.content_hash.clone(),
            activities: self.manifest.activities.clone(),
            input_schema: self.manifest.input_schema.clone(),
            output_schema: self.manifest.output_schema.clone(),
        }
    }

    /// Returns engine-ready deployed module names paired with their beam bytes.
    ///
    /// The engine performs the actual VM registration; this crate only supplies
    /// the validated namespaced names and exact module bytes.
    #[must_use]
    pub fn deployed_modules(&self) -> Vec<(String, &[u8])> {
        self.beams
            .iter()
            .map(|module| {
                (
                    deployed_name(module.name(), &self.content_hash),
                    module.bytes(),
                )
            })
            .collect()
    }

    /// Returns the deployed namespaced module name for the manifest entry module.
    #[must_use]
    pub fn deployed_entry_module(&self) -> String {
        deployed_name(&self.manifest.entry_module, &self.content_hash)
    }

    /// Re-serialises this validated package into canonical `.aion` archive
    /// bytes.
    ///
    /// The deterministic [`crate::PackageBuilder`] write path is used, so the
    /// output round-trips through [`Self::load_from_bytes`] to a package with
    /// the same content hash (the hash covers the beam set only), the same
    /// canonical manifest digest, and the same source set. This is the
    /// persistence form for runtime-deployed packages: the engine stores
    /// these bytes so a deploy survives restart.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError`] variants for manifest serialisation or ZIP
    /// writer failures; the entry module is already proven present by load
    /// validation.
    pub fn to_archive_bytes(&self) -> Result<Vec<u8>, PackageError> {
        crate::PackageBuilder::with_source(
            self.manifest.clone(),
            self.beams.clone(),
            self.source.clone(),
        )
        .write_to_bytes()
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    #[must_use]
    pub fn from_validated_parts_for_test(
        manifest: Manifest,
        beams: BeamSet,
        source: BTreeMap<String, Vec<u8>>,
        content_hash: ContentHash,
    ) -> Self {
        Self {
            manifest,
            beams,
            source,
            content_hash,
        }
    }
}

fn read_manifest<R>(
    archive: &mut ZipArchive<R>,
    budget: &mut ExtractionBudget,
) -> Result<Manifest, PackageError>
where
    R: Read + Seek,
{
    let mut manifest_file = match archive.by_name(MANIFEST_ENTRY) {
        Ok(file) => file,
        Err(ZipError::FileNotFound) => return Err(PackageError::MissingManifest),
        Err(error) => return Err(PackageError::ArchiveRead(error)),
    };

    let manifest_bytes = budget.read_entry(&mut manifest_file)?;

    serde_json::from_slice(&manifest_bytes).map_err(|source| PackageError::ManifestParse { source })
}

fn read_archive_entries<R>(
    archive: &mut ZipArchive<R>,
    budget: &mut ExtractionBudget,
) -> Result<(BeamSet, BTreeMap<String, Vec<u8>>), PackageError>
where
    R: Read + Seek,
{
    let mut modules = Vec::new();
    let mut source = BTreeMap::new();

    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(PackageError::ArchiveRead)?;
        if file.is_dir() {
            continue;
        }

        let entry = file.name().to_owned();
        if entry == MANIFEST_ENTRY {
            continue;
        }

        if entry.starts_with(BEAM_PREFIX) {
            let logical = logical_name_from_entry(&entry, BEAM_PREFIX, BEAM_SUFFIX)?;
            let bytes = budget.read_entry(&mut file)?;
            modules.push(BeamModule::new(logical, bytes));
        } else if entry.starts_with(SOURCE_PREFIX) {
            let logical = logical_name_from_entry(&entry, SOURCE_PREFIX, SOURCE_SUFFIX)?;
            let bytes = budget.read_entry(&mut file)?;
            if source.insert(logical, bytes).is_some() {
                return Err(PackageError::MalformedBeamEntry { entry });
            }
        }
    }

    let beams = BeamSet::new(modules)?;
    Ok((beams, source))
}

fn logical_name_from_entry(
    entry: &str,
    prefix: &str,
    suffix: &str,
) -> Result<String, PackageError> {
    let Some(without_prefix) = entry.strip_prefix(prefix) else {
        return Err(PackageError::MalformedBeamEntry {
            entry: entry.to_owned(),
        });
    };
    let Some(logical) = without_prefix.strip_suffix(suffix) else {
        return Err(PackageError::MalformedBeamEntry {
            entry: entry.to_owned(),
        });
    };

    if is_safe_logical_name(logical) {
        Ok(logical.to_owned())
    } else {
        Err(PackageError::MalformedBeamEntry {
            entry: entry.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
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

        let result =
            Package::load_from_bytes(&bytes, ExtractionLimits::bounded(inflated_total - 1));
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
    fn beam_entry_with_deployed_name_separator_returns_malformed_entry() -> Result<(), PackageError>
    {
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
    fn to_archive_bytes_round_trips_to_an_identical_package() -> Result<(), PackageError> {
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

        let reloaded = Package::load_from_bytes(
            package.to_archive_bytes()?,
            ExtractionLimits::unbounded(),
        )?;

        assert_eq!(reloaded, package);
        assert_eq!(
            reloaded.manifest().canonical_digest()?,
            package.manifest().canonical_digest()?
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
}
