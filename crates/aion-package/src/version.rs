//! Canonical workflow version record produced from a loaded `Package`.

use serde::{Deserialize, Serialize};

use crate::{ContentHash, DeclaredActivity};

/// Cross-system record describing a validated workflow package version.
///
/// The record is produced from a loaded [`crate::Package`] after archive
/// integrity has been verified. It carries the logical entry module and
/// recomputed content hash alongside the manifest-declared activities and
/// schemas. Stores that cannot depend on `aion-package` can persist the textual
/// content-hash form, but this is the canonical typed record for consumers that
/// do depend on this crate.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WorkflowVersion {
    /// Logical workflow entry module before deployed-name namespacing.
    pub entry_module: String,
    /// Recomputed SHA-256 content hash of the canonical beam set.
    pub content_hash: ContentHash,
    /// Activity types declared by the workflow manifest.
    pub activities: Vec<DeclaredActivity>,
    /// JSON schema accepted by the workflow entry point.
    pub input_schema: serde_json::Value,
    /// JSON schema produced by the workflow entry point.
    pub output_schema: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, time::Duration};

    use serde_json::json;

    use crate::{
        BeamModule, BeamSet, CURRENT_FORMAT_VERSION, DeclaredActivity, ExtractionLimits, Manifest,
        ManifestVersion, Package, PackageBuilder, PackageError,
    };

    fn sample_manifest() -> Manifest {
        Manifest {
            entry_module: "workflow/order".to_owned(),
            entry_function: "run".to_owned(),
            input_schema: json!({
                "type": "object",
                "required": ["order_id"],
                "properties": {
                    "order_id": { "type": "string" }
                }
            }),
            output_schema: json!({
                "type": "object",
                "required": ["status"],
                "properties": {
                    "status": { "type": "string" }
                }
            }),
            timeout: Some(Duration::from_secs(30)),
            activities: vec![
                DeclaredActivity {
                    activity_type: "charge_card".to_owned(),
                },
                DeclaredActivity {
                    activity_type: "send_receipt".to_owned(),
                },
            ],
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

    #[test]
    fn loaded_package_produces_matching_version_record() -> Result<(), PackageError> {
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

        let record = package.version_record();

        assert_eq!(record.entry_module, package.manifest().entry_module);
        assert_eq!(record.content_hash, package.content_hash().clone());
        assert_eq!(record.activities, package.manifest().activities);
        assert_eq!(record.input_schema, package.manifest().input_schema);
        assert_eq!(record.output_schema, package.manifest().output_schema);
        Ok(())
    }
}
