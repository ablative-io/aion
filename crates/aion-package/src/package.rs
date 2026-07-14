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
    builder::is_safe_logical_name,
    extraction::ExtractionBudget,
    hash::{has_explicit_timeout_identity, verified_content_hash},
    namespace::deployed_name,
    version::WorkflowVersion,
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
        let content_hash = verified_content_hash(&beams, &manifest)?;

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
    /// the same legacy or explicit-timeout content hash, canonical manifest
    /// digest, and source set. This is the persistence form for runtime-deployed
    /// packages: the engine stores these bytes so a deploy survives restart.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError`] variants for manifest serialisation or ZIP
    /// writer failures; the entry module is already proven present by load
    /// validation.
    pub fn to_archive_bytes(&self) -> Result<Vec<u8>, PackageError> {
        let mut builder = crate::PackageBuilder::with_source(
            self.manifest.clone(),
            self.beams.clone(),
            self.source.clone(),
        );
        if has_explicit_timeout_identity(&self.beams, &self.manifest, &self.content_hash) {
            builder = builder.with_explicit_timeout_identity();
        }
        builder.write_to_bytes()
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
#[path = "package_tests.rs"]
mod tests;
