//! Operator deploy API serde/prost wire types.
//!
//! These shapes mirror `proto/deploy.proto` and double as the JSON bodies of
//! the `/deploy/*` HTTP routes (the archive upload itself rides a raw
//! `application/octet-stream` body, not JSON). Deploy is an operator
//! surface: it is deliberately separate from the workflow wire types so the
//! caller SDK contract never grows deploy operations, and it is
//! engine-global — no namespace field exists anywhere here.

/// Proto representation of `LoadPackageRequest`: one complete `.aion`
/// archive uploaded in a single unary message.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoLoadPackageRequest {
    /// Complete `.aion` archive bytes.
    #[prost(bytes = "vec", tag = "1")]
    pub archive: Vec<u8>,
}

/// Proto representation of `LoadPackageResponse`.
///
/// Idempotency is specified behavior: re-sending the same archive succeeds
/// with `freshly_loaded = false`, and `route_changed` reports whether the
/// call re-pointed routing. A deploy pipeline may retry blindly.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoLoadPackageResponse {
    /// Logical workflow type the package registers.
    #[prost(string, tag = "1")]
    pub workflow_type: String,
    /// Textual content hash — the version identifier everywhere on the wire.
    #[prost(string, tag = "2")]
    pub content_hash: String,
    /// Namespaced module name spawned for this version.
    #[prost(string, tag = "3")]
    pub deployed_entry_module: String,
    /// Exported entry function spawned for this version.
    #[prost(string, tag = "4")]
    pub entry_function: String,
    /// False = idempotent re-load (the hash was already resident).
    #[prost(bool, tag = "5")]
    pub freshly_loaded: bool,
    /// False = the hash was already route-active (full no-op).
    #[prost(bool, tag = "6")]
    pub route_changed: bool,
}

/// Proto representation of `ListVersionsRequest` (engine-global; no namespace).
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoListVersionsRequest {}

/// Proto representation of one loaded `WorkflowVersion` listing row.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoWorkflowVersion {
    /// Logical workflow type this version belongs to.
    #[prost(string, tag = "1")]
    pub workflow_type: String,
    /// Textual content hash identifying the package version.
    #[prost(string, tag = "2")]
    pub content_hash: String,
    /// Namespaced module name spawned for this version.
    #[prost(string, tag = "3")]
    pub deployed_entry_module: String,
    /// Exported entry function spawned for this version.
    #[prost(string, tag = "4")]
    pub entry_function: String,
    /// Author-declared manifest version label.
    #[prost(string, tag = "5")]
    pub manifest_version: String,
    /// RFC 3339 engine-local load instant.
    #[prost(string, tag = "6")]
    pub loaded_at: String,
    /// Whether new dispatches of this type currently route to this version.
    #[prost(bool, tag = "7")]
    pub route_active: bool,
}

/// Proto representation of `ListVersionsResponse`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoListVersionsResponse {
    /// Every loaded version with its routing flag, sorted `(type, loaded_at)`.
    #[prost(message, repeated, tag = "1")]
    pub versions: Vec<ProtoWorkflowVersion>,
}

/// Proto representation of `RouteVersionRequest` (rollback / roll-forward).
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoRouteVersionRequest {
    /// Logical workflow type whose route is re-pointed.
    #[prost(string, tag = "1")]
    pub workflow_type: String,
    /// Textual content hash of the already-loaded target version.
    #[prost(string, tag = "2")]
    pub content_hash: String,
}

/// Proto representation of `RouteVersionResponse`.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoRouteVersionResponse {}

/// Proto representation of `UnloadVersionRequest`.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoUnloadVersionRequest {
    /// Logical workflow type whose version is unloaded.
    #[prost(string, tag = "1")]
    pub workflow_type: String,
    /// Textual content hash of the non-routed version to unload.
    #[prost(string, tag = "2")]
    pub content_hash: String,
}

/// Proto representation of `UnloadVersionResponse`.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, prost::Message)]
pub struct ProtoUnloadVersionResponse {}

#[cfg(test)]
mod tests {
    use super::{ProtoLoadPackageResponse, ProtoRouteVersionRequest, ProtoWorkflowVersion};

    #[test]
    fn deploy_shapes_round_trip_through_json() -> Result<(), serde_json::Error> {
        let response = ProtoLoadPackageResponse {
            workflow_type: "order".to_owned(),
            content_hash: "a".repeat(64),
            deployed_entry_module: format!("order${}", "a".repeat(64)),
            entry_function: "run".to_owned(),
            freshly_loaded: true,
            route_changed: true,
        };
        let decoded: ProtoLoadPackageResponse =
            serde_json::from_str(&serde_json::to_string(&response)?)?;
        assert_eq!(decoded, response);

        let route = ProtoRouteVersionRequest {
            workflow_type: "order".to_owned(),
            content_hash: "b".repeat(64),
        };
        let decoded: ProtoRouteVersionRequest =
            serde_json::from_str(&serde_json::to_string(&route)?)?;
        assert_eq!(decoded, route);

        let version = ProtoWorkflowVersion {
            workflow_type: "order".to_owned(),
            content_hash: "c".repeat(64),
            deployed_entry_module: format!("order${}", "c".repeat(64)),
            entry_function: "run".to_owned(),
            manifest_version: "c".repeat(64),
            loaded_at: "2026-06-12T00:00:00Z".to_owned(),
            route_active: false,
        };
        let decoded: ProtoWorkflowVersion =
            serde_json::from_str(&serde_json::to_string(&version)?)?;
        assert_eq!(decoded, version);
        Ok(())
    }
}
