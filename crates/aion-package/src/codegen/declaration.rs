//! Parsed activity declarations: the typed Gleam source, read as JSON.
//!
//! The typed Gleam activity declaration is the single source of truth
//! (ADR-014). Because a Rust generator cannot read Gleam types, `aion generate`
//! runs the package's `manifest()` export (via the `gleam` toolchain, in the
//! CLI — this library never spawns a process) and feeds the canonical JSON it
//! prints to [`parse_declarations`]. The result drives the activity-plumbing
//! codegen the way parsed schemas drive codec codegen.
//!
//! This module is pure: it validates the declaration list (name safety,
//! uniqueness, known tier) and preserves declaration order, which is
//! load-bearing — it fixes the order of generated wrappers, registration
//! entries, and the `workflow.toml` activities list, so a byte-identical
//! round-trip depends on it. Resolving a declared value type to its schema is
//! deferred to generation.

use std::collections::HashSet;

use serde::Deserialize;

use super::error::CodegenError;
use super::names::{is_reserved_word, is_snake_identifier};

/// Where an activity's side-effecting body executes.
///
/// Mirrors the Gleam `activity.Tier`. The generator reads it to choose the
/// worker handler stub and registration entry to emit, and whether a
/// wire-compat golden is generated (remote tiers only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Runs in-process inside the BEAM VM via a registered NIF.
    InVm,
    /// Runs in a remote Python worker over the worker protocol.
    RemotePython,
    /// Runs in a remote Rust worker over the worker protocol.
    RemoteRust,
}

impl Tier {
    /// Parses the canonical wire string emitted by Gleam `tier_to_string`.
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "in_vm" => Some(Self::InVm),
            "remote_python" => Some(Self::RemotePython),
            "remote_rust" => Some(Self::RemoteRust),
            _ => None,
        }
    }

    /// Whether the activity's body lives in a remote worker (so a wire-compat
    /// golden is generated for it).
    #[must_use]
    pub fn is_remote(self) -> bool {
        matches!(self, Self::RemotePython | Self::RemoteRust)
    }
}

/// A validated activity declaration: the per-activity facts the generator needs.
///
/// `input_type` and `output_type` are the value type names the author wrote in
/// the Gleam declaration (for example `OrderInput`); generation resolves each to
/// the `schemas/*.json` document whose generated type carries that name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityDeclaration {
    /// The engine-facing activity name.
    pub name: String,
    /// The tier the activity runs on.
    pub tier: Tier,
    /// The input value type name.
    pub input_type: String,
    /// The output value type name.
    pub output_type: String,
}

/// One declaration as it appears in the manifest JSON. Unknown fields are
/// rejected so a typo in the wire contract fails loudly rather than silently
/// dropping data.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDeclaration {
    name: String,
    tier: String,
    input: String,
    output: String,
}

/// Parses and validates the canonical activity-manifest JSON.
///
/// # Errors
///
/// Returns [`CodegenError::ManifestParse`] if the bytes are not a JSON array of
/// declaration objects, [`CodegenError::InvalidActivityName`] for a name that
/// cannot name an engine activity, [`CodegenError::DuplicateActivity`] for a
/// repeated name, and [`CodegenError::UnknownTier`] for an unrecognised tier.
/// Declaration order is preserved.
pub fn parse_declarations(json: &[u8]) -> Result<Vec<ActivityDeclaration>, CodegenError> {
    let raw: Vec<RawDeclaration> =
        serde_json::from_slice(json).map_err(|source| CodegenError::ManifestParse { source })?;

    let mut declarations = Vec::with_capacity(raw.len());
    let mut seen: HashSet<&str> = HashSet::with_capacity(raw.len());
    for entry in &raw {
        if !is_snake_identifier(&entry.name) || is_reserved_word(&entry.name) {
            return Err(CodegenError::InvalidActivityName {
                name: entry.name.clone(),
                reason: "must be a snake_case identifier (a lowercase letter followed by \
                         lowercase letters, digits, or underscores) and not a Gleam reserved \
                         word, so it can name the generated wrapper function and worker handler"
                    .to_owned(),
            });
        }
        if !seen.insert(entry.name.as_str()) {
            return Err(CodegenError::DuplicateActivity {
                name: entry.name.clone(),
            });
        }
        let tier = Tier::from_wire(&entry.tier).ok_or_else(|| CodegenError::UnknownTier {
            value: entry.tier.clone(),
        })?;
        declarations.push(ActivityDeclaration {
            name: entry.name.clone(),
            tier,
            input_type: entry.input.clone(),
            output_type: entry.output.clone(),
        });
    }
    Ok(declarations)
}

#[cfg(test)]
mod tests {
    use super::{Tier, parse_declarations};
    use crate::codegen::error::CodegenError;

    const WIRE: &[u8] = br#"[
        {"name":"reserve_inventory","tier":"remote_python","input":"OrderInput","output":"InventoryReservation"},
        {"name":"ship_order","tier":"in_vm","input":"OrderInput","output":"Shipment"}
    ]"#;

    #[test]
    fn parses_in_declaration_order() -> Result<(), CodegenError> {
        let declarations = parse_declarations(WIRE)?;
        assert_eq!(declarations.len(), 2);
        assert_eq!(declarations[0].name, "reserve_inventory");
        assert_eq!(declarations[0].tier, Tier::RemotePython);
        assert!(declarations[0].tier.is_remote());
        assert_eq!(declarations[0].input_type, "OrderInput");
        assert_eq!(declarations[0].output_type, "InventoryReservation");
        assert_eq!(declarations[1].name, "ship_order");
        assert_eq!(declarations[1].tier, Tier::InVm);
        assert!(!declarations[1].tier.is_remote());
        Ok(())
    }

    #[test]
    fn empty_manifest_parses_to_no_declarations() -> Result<(), CodegenError> {
        assert!(parse_declarations(b"[]")?.is_empty());
        Ok(())
    }

    #[test]
    fn unknown_tier_is_rejected() {
        let json = br#"[{"name":"x","tier":"remote_go","input":"A","output":"B"}]"#;
        assert!(matches!(
            parse_declarations(json),
            Err(CodegenError::UnknownTier { value }) if value == "remote_go"
        ));
    }

    #[test]
    fn duplicate_activity_is_rejected() {
        let json = br#"[
            {"name":"x","tier":"in_vm","input":"A","output":"B"},
            {"name":"x","tier":"in_vm","input":"A","output":"B"}
        ]"#;
        assert!(matches!(
            parse_declarations(json),
            Err(CodegenError::DuplicateActivity { name }) if name == "x"
        ));
    }

    #[test]
    fn names_that_cannot_derive_an_identifier_are_rejected() {
        // The activity name becomes the generated Gleam wrapper function and the
        // worker handler, so anything that is not a snake_case identifier — a
        // path separator, an uppercase letter, a hyphen, a leading digit — or a
        // Gleam reserved word must fail loudly here, not as a later build error.
        for bad in [
            "a/b",
            "ReserveInventory",
            "reserve-inventory",
            "1st",
            "type",
            "",
        ] {
            let json = format!(r#"[{{"name":"{bad}","tier":"in_vm","input":"A","output":"B"}}]"#);
            assert!(
                matches!(
                    parse_declarations(json.as_bytes()),
                    Err(CodegenError::InvalidActivityName { name, .. }) if name == bad
                ),
                "expected InvalidActivityName for `{bad}`"
            );
        }
    }

    #[test]
    fn well_formed_snake_case_names_are_accepted() -> Result<(), CodegenError> {
        let json = br#"[{"name":"reserve_inventory_v2","tier":"in_vm","input":"A","output":"B"}]"#;
        assert_eq!(parse_declarations(json)?[0].name, "reserve_inventory_v2");
        Ok(())
    }

    #[test]
    fn malformed_json_is_rejected() {
        assert!(matches!(
            parse_declarations(b"not json"),
            Err(CodegenError::ManifestParse { .. })
        ));
    }
}
