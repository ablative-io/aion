//! Remote operator deploy subcommands over the gRPC `DeployService`.
//!
//! The deploy stub comes from `aion-proto`'s generated module — never from
//! `aion-client`, whose caller-SDK surface is contract-bound and must not
//! grow operator operations. Token sourcing: `--token` overrides the
//! `AION_TOKEN` environment variable; when neither is present the CLI relies
//! on the server's development paths and sends the `x-aion-deploy: true`
//! header alongside `x-aion-subject`.

use std::path::Path;

use aion_proto::generated::deploy_service_client::DeployServiceClient;
use aion_proto::{ProtoWireError, WireError, generated};
use anyhow::{Context, Result};
use prost::Message as _;
use serde_json::{Value, json};
use tonic::transport::Channel;

/// Connection facts shared by every deploy operation.
pub(crate) struct DeployTarget {
    endpoint: String,
    subject: String,
    token: Option<String>,
}

impl DeployTarget {
    pub(crate) fn new(endpoint: String, subject: String, token: Option<String>) -> Self {
        Self {
            endpoint,
            subject,
            token,
        }
    }

    async fn client(&self) -> Result<DeployServiceClient<Channel>> {
        let channel = tonic::transport::Endpoint::try_from(self.endpoint.clone())
            .context("invalid --endpoint")?
            .connect()
            .await
            .context("failed to connect to Aion server")?;
        Ok(DeployServiceClient::new(channel))
    }

    /// Wraps a message with the deploy credential metadata: the bearer token
    /// when one is sourced, plus the development subject/deploy headers
    /// (ignored by the JWT path, authoritative in dev modes).
    fn request<T>(&self, message: T) -> Result<tonic::Request<T>> {
        let mut request = tonic::Request::new(message);
        let metadata = request.metadata_mut();
        metadata.insert(
            "x-aion-subject",
            self.subject
                .parse()
                .context("--subject is not valid metadata")?,
        );
        metadata.insert(
            "x-aion-deploy",
            "true".parse().context("static metadata must parse")?,
        );
        if let Some(token) = &self.token {
            metadata.insert(
                "authorization",
                format!("Bearer {token}")
                    .parse()
                    .context("--token is not valid metadata")?,
            );
        }
        Ok(request)
    }
}

/// Resolves the deploy bearer token: `--token` wins over `AION_TOKEN`.
pub(crate) fn resolve_token(flag: Option<String>) -> Option<String> {
    resolve_token_from(flag, std::env::var("AION_TOKEN").ok())
}

/// Pure precedence rule: the flag wins; empty environment values are absent.
fn resolve_token_from(flag: Option<String>, env: Option<String>) -> Option<String> {
    flag.or_else(|| env.filter(|token| !token.is_empty()))
}

/// `aion-cli deploy <archive.aion>`: reads and uploads a complete archive.
pub(crate) async fn deploy(target: &DeployTarget, archive_path: &Path) -> Result<Value> {
    let archive = std::fs::read(archive_path)
        .with_context(|| format!("failed to read archive `{}`", archive_path.display()))?;
    deploy_bytes(target, archive).await
}

/// Uploads complete package bytes through the one operator deploy path shared
/// by `.aion` archives and direct-compiled `.awl` documents.
pub(crate) async fn deploy_bytes(target: &DeployTarget, archive: Vec<u8>) -> Result<Value> {
    let mut client = target.client().await?;
    let response = client
        .load_package(target.request(generated::LoadPackageRequest { archive })?)
        .await
        .map_err(|status| deploy_status_error(&status))
        .context("failed to deploy package")?
        .into_inner();
    Ok(json!({
        "workflow_type": response.workflow_type,
        "content_hash": response.content_hash,
        "deployed_entry_module": response.deployed_entry_module,
        "entry_function": response.entry_function,
        "freshly_loaded": response.freshly_loaded,
        "route_changed": response.route_changed,
    }))
}

/// `aion-cli versions [--workflow-type T]`: the deploy read model, with a
/// client-side type filter.
pub(crate) async fn versions(target: &DeployTarget, workflow_type: Option<&str>) -> Result<Value> {
    let mut client = target.client().await?;
    let response = client
        .list_versions(target.request(generated::ListVersionsRequest {})?)
        .await
        .map_err(|status| deploy_status_error(&status))
        .context("failed to list workflow versions")?
        .into_inner();
    let versions: Vec<Value> = response
        .versions
        .into_iter()
        .filter(|version| workflow_type.is_none_or(|filter| version.workflow_type == filter))
        .map(|version| {
            json!({
                "workflow_type": version.workflow_type,
                "content_hash": version.content_hash,
                "deployed_entry_module": version.deployed_entry_module,
                "entry_function": version.entry_function,
                "manifest_version": version.manifest_version,
                "loaded_at": version.loaded_at,
                "route_active": version.route_active,
            })
        })
        .collect();
    Ok(Value::Array(versions))
}

/// `aion-cli route <workflow-type> <content-hash>`: rollback / roll-forward.
pub(crate) async fn route(
    target: &DeployTarget,
    workflow_type: &str,
    content_hash: &str,
) -> Result<Value> {
    let mut client = target.client().await?;
    client
        .route_version(target.request(generated::RouteVersionRequest {
            workflow_type: workflow_type.to_owned(),
            content_hash: content_hash.to_owned(),
        })?)
        .await
        .map_err(|status| deploy_status_error(&status))
        .context("failed to route workflow version")?;
    Ok(json!({
        "workflow_type": workflow_type,
        "content_hash": content_hash,
        "route_active": true,
    }))
}

/// `aion-cli unload <workflow-type> <content-hash>`.
pub(crate) async fn unload(
    target: &DeployTarget,
    workflow_type: &str,
    content_hash: &str,
) -> Result<Value> {
    let mut client = target.client().await?;
    client
        .unload_version(target.request(generated::UnloadVersionRequest {
            workflow_type: workflow_type.to_owned(),
            content_hash: content_hash.to_owned(),
        })?)
        .await
        .map_err(|status| deploy_status_error(&status))
        .context("failed to unload workflow version")?;
    Ok(json!({
        "workflow_type": workflow_type,
        "content_hash": content_hash,
        "unloaded": true,
    }))
}

/// Surfaces the typed `WireError` detail riding a deploy status so the
/// renderer can branch on `deploy_denied` / `version_pinned`; statuses
/// without a decodable detail keep the tonic code and message.
fn deploy_status_error(status: &tonic::Status) -> anyhow::Error {
    match ProtoWireError::decode(status.details()) {
        Ok(proto) => match WireError::try_from(proto) {
            Ok(wire) => anyhow::Error::new(wire),
            Err(fallback) => anyhow::Error::new(fallback),
        },
        Err(_) => anyhow::anyhow!("grpc {:?}: {}", status.code(), status.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_token_from;

    /// `--token` wins over `AION_TOKEN`; the environment fills in only when
    /// the flag is absent; empty environment values are ignored.
    #[test]
    fn token_resolution_prefers_the_flag() {
        assert_eq!(
            resolve_token_from(Some("from-flag".to_owned()), Some("from-env".to_owned()))
                .as_deref(),
            Some("from-flag")
        );
        assert_eq!(
            resolve_token_from(None, Some("from-env".to_owned())).as_deref(),
            Some("from-env")
        );
        assert_eq!(resolve_token_from(None, Some(String::new())), None);
        assert_eq!(resolve_token_from(None, None), None);
    }
}
