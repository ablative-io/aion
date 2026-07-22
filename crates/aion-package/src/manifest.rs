//! Typed `manifest.json` model and `.aion` format-version checks.

use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::PackageError;

/// Canonical SHA-256 digest of one manifest's serialized JSON form.
///
/// Legacy package versions cover the canonical beam set only; packages that
/// opt an explicit workflow timeout into identity additionally cover that
/// timeout. Other `manifest.json` fields remain excluded, so this digest is the
/// tripwire for divergent manifests that still carry one version: the engine
/// catalog retains it and refuses a mismatched idempotent reload.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ManifestDigest([u8; 32]);

impl ManifestDigest {
    /// Creates a manifest digest from raw SHA-256 digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the raw SHA-256 digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ManifestDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Current `.aion` manifest and archive-layout schema version supported by this crate.
pub const CURRENT_FORMAT_VERSION: u32 = 1;

/// Textual content-hash version stored in `manifest.json`.
///
/// The content hash is computed and stamped by later package-building code; this
/// type keeps the manifest field distinct from unrelated strings while this
/// module remains side-effect free.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestVersion(pub String);

impl ManifestVersion {
    /// Creates a manifest version value from the hash's stable textual form.
    #[must_use]
    pub fn new(version: impl Into<String>) -> Self {
        Self(version.into())
    }

    /// Returns the stored textual content-hash version.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Activity declaration recorded in the package manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredActivity {
    /// Stable `activity_type` key naming an activity type invoked by workflow code.
    ///
    /// This follows the current `aion-core` event convention, where scheduled
    /// activity types are represented as strings.
    #[serde(rename = "activity_type")]
    pub activity_type: String,
}

/// An additional workflow entry exported by the same `.aion` archive.
///
/// Additional entries share the archive's content hash and BEAM closure with
/// the primary manifest entry. `workflow_type` is the routing name, while
/// `entry_module` and `entry_function` identify the callable in that closure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkflowEntry {
    /// Stable workflow type used by start and child-spawn routing.
    pub workflow_type: String,
    /// Logical BEAM module exporting the entry function.
    pub entry_module: String,
    /// Exported engine entry function.
    pub entry_function: String,
    /// JSON Schema for the entry's input payload.
    pub input_schema: serde_json::Value,
    /// JSON Schema for the entry's result payload.
    pub output_schema: serde_json::Value,
    /// Explicitly authored workflow execution timeout, or `None` when the entry
    /// declared none. Serialised only when present, so a manifest written for an
    /// entry with no authored timeout carries no `timeout` key at all; a legacy
    /// manifest that still carries a defaulted value decodes to `Some(_)` but is
    /// held non-arming by the package's content-hash identity (see
    /// [`crate::Package`]). Never a buried default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Duration>,
    /// Whether this entry is package-internal rather than operator-authored.
    #[serde(default)]
    pub internal: bool,
}

/// Typed on-disk `manifest.json` descriptor for a `.aion` package.
///
/// The public field names are the stable JSON keys written into `manifest.json`:
/// `entry_module`, `entry_function`, `input_schema`, `output_schema`, `timeout`,
/// `activities`, `version`, and `format_version`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Stable `entry_module` key naming the logical workflow entry module.
    #[serde(rename = "entry_module")]
    pub entry_module: String,
    /// Stable `entry_function` key naming the exported workflow entry function.
    #[serde(rename = "entry_function")]
    pub entry_function: String,
    /// Stable `input_schema` key containing a JSON-Schema document for input payloads.
    #[serde(rename = "input_schema")]
    pub input_schema: serde_json::Value,
    /// Stable `output_schema` key containing a JSON-Schema document for result payloads.
    #[serde(rename = "output_schema")]
    pub output_schema: serde_json::Value,
    /// Stable `timeout` key: the explicitly authored workflow timeout, or absent.
    ///
    /// `None` is serialised as an omitted key — a manifest written for a workflow
    /// with no authored timeout carries no `timeout` at all, so nothing is armed.
    /// A legacy manifest that carries a defaulted duration decodes to `Some(_)`,
    /// but arming is authorised by the package's content-hash identity, not by
    /// this field's presence: a legacy (beams-only) identity reads as not
    /// declared regardless (see [`crate::Package::has_declared_timeout`]). This
    /// key therefore never encodes a buried default.
    #[serde(rename = "timeout", default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Duration>,
    /// Stable `activities` key listing activity types declared by the workflow.
    #[serde(rename = "activities")]
    pub activities: Vec<DeclaredActivity>,
    /// Stable `version` key containing the package content hash textual value.
    #[serde(rename = "version")]
    pub version: ManifestVersion,
    /// Stable `format_version` key identifying the `.aion` format schema version.
    ///
    /// This lets future layout changes be detected rather than silently misread.
    #[serde(rename = "format_version")]
    pub format_version: u32,
    /// Additional workflow entries exported by this same archive and pinned to
    /// the same content hash. Absent in packages created before multi-entry
    /// registration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_workflows: Vec<WorkflowEntry>,
}

impl Manifest {
    /// Checks whether this manifest declares a supported `.aion` format version.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::UnknownFormatVersion`] when `format_version` is
    /// not [`CURRENT_FORMAT_VERSION`].
    pub fn check_format_version(&self) -> Result<(), PackageError> {
        if self.format_version == CURRENT_FORMAT_VERSION {
            Ok(())
        } else {
            Err(PackageError::UnknownFormatVersion {
                found: self.format_version,
            })
        }
    }

    /// Computes the canonical SHA-256 digest of this manifest.
    ///
    /// The digest covers the manifest's stable serialized JSON form (the same
    /// field names and ordering written into `manifest.json`), so any
    /// semantic difference — entry function, schemas, timeout, declared
    /// activities — produces a different digest even when the beam set (and
    /// therefore the content hash) is unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::ManifestSerialise`] when the manifest cannot be
    /// serialized to JSON.
    pub fn canonical_digest(&self) -> Result<ManifestDigest, PackageError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|source| PackageError::ManifestSerialise { source })?;
        let mut digest = Sha256::new();
        digest.update(&bytes);
        Ok(ManifestDigest(digest.finalize().into()))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{
        CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion, WorkflowEntry,
    };
    use crate::PackageError;

    fn sample_manifest() -> Manifest {
        Manifest {
            entry_module: "workflow/order".to_owned(),
            entry_function: "run".to_owned(),
            input_schema: json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "required": ["order_id"],
                "properties": {
                    "order_id": { "type": "string" },
                    "retry": { "type": "boolean" }
                }
            }),
            output_schema: json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "required": ["status"],
                "properties": {
                    "status": { "enum": ["accepted", "rejected"] },
                    "total": { "type": "number" }
                }
            }),
            timeout: Some(Duration::new(30, 250_000_000)),
            activities: vec![
                DeclaredActivity {
                    activity_type: "charge_card".to_owned(),
                },
                DeclaredActivity {
                    activity_type: "send_receipt".to_owned(),
                },
            ],
            version: ManifestVersion::new(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            ),
            format_version: CURRENT_FORMAT_VERSION,
            additional_workflows: Vec::new(),
        }
    }

    #[test]
    fn manifest_round_trips_losslessly_through_json() -> Result<(), serde_json::Error> {
        let manifest = sample_manifest();

        let json = serde_json::to_string(&manifest)?;
        let decoded: Manifest = serde_json::from_str(&json)?;

        assert_eq!(decoded, manifest);
        Ok(())
    }

    #[test]
    fn absent_additional_workflow_list_loads_as_legacy_empty() -> Result<(), serde_json::Error> {
        let mut value = serde_json::to_value(sample_manifest())?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| serde_json::Error::io(std::io::Error::other("manifest not object")))?;
        object.remove("additional_workflows");
        let decoded: Manifest = serde_json::from_value(value)?;
        assert!(decoded.additional_workflows.is_empty());
        Ok(())
    }

    #[test]
    fn additional_workflow_entry_round_trips_with_internal_flag() -> Result<(), serde_json::Error> {
        let mut manifest = sample_manifest();
        manifest.additional_workflows.push(WorkflowEntry {
            workflow_type: "awl_distribute_items_0".to_owned(),
            entry_module: manifest.entry_module.clone(),
            entry_function: "awl_distribute_items_0_run".to_owned(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "string" }),
            timeout: Some(Duration::from_secs(30)),
            internal: true,
        });
        let encoded = serde_json::to_string(&manifest)?;
        let decoded: Manifest = serde_json::from_str(&encoded)?;
        assert_eq!(decoded, manifest);
        assert!(
            decoded
                .additional_workflows
                .first()
                .is_some_and(|entry| entry.internal)
        );
        Ok(())
    }

    #[test]
    fn manifest_with_schemas_and_declared_activities_round_trips() -> Result<(), serde_json::Error>
    {
        let manifest = sample_manifest();

        let json = serde_json::to_string(&manifest)?;
        let decoded: Manifest = serde_json::from_str(&json)?;

        assert_eq!(
            decoded.input_schema["properties"]["order_id"]["type"],
            "string"
        );
        assert_eq!(
            decoded.output_schema["properties"]["status"]["enum"][0],
            "accepted"
        );
        assert_eq!(decoded.activities.len(), 2);
        assert_eq!(decoded, manifest);
        Ok(())
    }

    #[test]
    fn supported_format_version_passes() -> Result<(), PackageError> {
        sample_manifest().check_format_version()
    }

    /// Identical manifests digest identically; any semantic change (entry
    /// function here) changes the digest even though the beam set — and
    /// therefore the content hash — is untouched.
    #[test]
    fn canonical_digest_detects_manifest_divergence() -> Result<(), PackageError> {
        let manifest = sample_manifest();
        let same = sample_manifest();
        let mut diverged = sample_manifest();
        diverged.entry_function = "start".to_owned();

        assert_eq!(manifest.canonical_digest()?, same.canonical_digest()?);
        assert_ne!(manifest.canonical_digest()?, diverged.canonical_digest()?);
        Ok(())
    }

    #[test]
    fn canonical_digest_renders_as_lowercase_hex() -> Result<(), PackageError> {
        let digest = sample_manifest().canonical_digest()?;
        let text = digest.to_string();

        assert_eq!(text.len(), 64);
        assert!(
            text.bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        );
        Ok(())
    }

    #[test]
    fn unsupported_format_version_returns_typed_error() {
        let mut manifest = sample_manifest();
        manifest.format_version = CURRENT_FORMAT_VERSION + 1;

        let result = manifest.check_format_version();

        assert!(matches!(
            result,
            Err(PackageError::UnknownFormatVersion { found }) if found == CURRENT_FORMAT_VERSION + 1
        ));
    }

    #[test]
    fn manifest_json_keys_are_stable() -> Result<(), serde_json::Error> {
        let manifest = sample_manifest();

        let json = serde_json::to_value(&manifest)?;

        assert!(json.get("entry_module").is_some());
        assert!(json.get("entry_function").is_some());
        assert!(json.get("input_schema").is_some());
        assert!(json.get("output_schema").is_some());
        assert!(json.get("timeout").is_some());
        assert!(json.get("activities").is_some());
        assert!(json.get("version").is_some());
        assert!(json.get("format_version").is_some());
        assert_eq!(json["activities"][0]["activity_type"], "charge_card");
        Ok(())
    }
}
