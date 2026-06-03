//! Typed `manifest.json` model and `.aion` format-version checks.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::PackageError;

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
    pub activity_type: String,
}

/// Typed on-disk `manifest.json` descriptor for a `.aion` package.
///
/// The public field names are the stable JSON keys written into `manifest.json`:
/// `entry_module`, `entry_function`, `input_schema`, `output_schema`, `timeout`,
/// `activities`, `version`, and `format_version`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Stable `entry_module` key naming the logical workflow entry module.
    pub entry_module: String,
    /// Stable `entry_function` key naming the exported workflow entry function.
    pub entry_function: String,
    /// Stable `input_schema` key containing a JSON-Schema document for input payloads.
    pub input_schema: serde_json::Value,
    /// Stable `output_schema` key containing a JSON-Schema document for result payloads.
    pub output_schema: serde_json::Value,
    /// Stable `timeout` key containing the workflow timeout as a serde-encoded duration.
    pub timeout: Duration,
    /// Stable `activities` key listing activity types declared by the workflow.
    pub activities: Vec<DeclaredActivity>,
    /// Stable `version` key containing the package content hash textual value.
    pub version: ManifestVersion,
    /// Stable `format_version` key identifying the `.aion` format schema version.
    ///
    /// This lets future layout changes be detected rather than silently misread.
    pub format_version: u32,
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
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{CURRENT_FORMAT_VERSION, DeclaredActivity, Manifest, ManifestVersion};
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
            timeout: Duration::new(30, 250_000_000),
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
}
