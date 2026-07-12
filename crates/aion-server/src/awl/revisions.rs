use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

const STATE_DIR: &str = ".aion-authoring";
const REVISION_DIR: &str = "revisions";
const DEPLOYMENT_DIR: &str = "deployments";
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Revision {
    pub content_hash: String,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DeploymentRecord {
    pub deployment_id: String,
    pub document_path: String,
    pub content_hash: String,
    pub package_id: String,
    pub workflow_type: String,
    pub task_queue: String,
    pub workflow_id: Option<String>,
    pub run_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RevisionError {
    #[error("invalid content hash: {0}")]
    InvalidHash(String),
    #[error("document revision was not found: {0}")]
    NotFound(String),
    #[error("revision store I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("deployment record is invalid: {0}")]
    InvalidRecord(String),
}

#[must_use]
pub fn content_hash(source: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(source.as_bytes());
    let bytes = digest.finalize();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

pub async fn store(root: &Path, source: &str) -> Result<Revision, RevisionError> {
    let revision = Revision {
        content_hash: content_hash(source),
        source: source.to_owned(),
    };
    let directory = root.join(STATE_DIR).join(REVISION_DIR);
    tokio::fs::create_dir_all(&directory).await?;
    let destination = directory.join(&revision.content_hash);
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .await
    {
        Ok(mut file) => {
            file.write_all(source.as_bytes()).await?;
            file.sync_all().await?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = tokio::fs::read_to_string(&destination).await?;
            if existing != source {
                return Err(RevisionError::InvalidRecord(format!(
                    "hash collision at {}",
                    revision.content_hash
                )));
            }
        }
        Err(error) => return Err(RevisionError::Io(error)),
    }
    Ok(revision)
}

pub async fn fetch(root: &Path, hash: &str) -> Result<Revision, RevisionError> {
    validate_hash(hash)?;
    let path = root.join(STATE_DIR).join(REVISION_DIR).join(hash);
    let source = tokio::fs::read_to_string(path).await.map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            RevisionError::NotFound(hash.to_owned())
        } else {
            RevisionError::Io(error)
        }
    })?;
    if content_hash(&source) != hash {
        return Err(RevisionError::InvalidRecord(format!(
            "stored revision {hash} failed content verification"
        )));
    }
    Ok(Revision {
        content_hash: hash.to_owned(),
        source,
    })
}

pub async fn record_deployment(
    root: &Path,
    record: &DeploymentRecord,
) -> Result<(), RevisionError> {
    validate_identifier(&record.deployment_id, "deployment id")?;
    validate_hash(&record.content_hash)?;
    let revision = fetch(root, &record.content_hash).await?;
    if revision.content_hash != record.content_hash {
        return Err(RevisionError::InvalidRecord(
            "deployment revision identity changed".to_owned(),
        ));
    }
    let directory = root.join(STATE_DIR).join(DEPLOYMENT_DIR);
    tokio::fs::create_dir_all(&directory).await?;
    let destination = directory.join(format!("{}.json", record.deployment_id));
    write_json_atomic(&destination, record).await
}

pub async fn deployment(
    root: &Path,
    deployment_id: &str,
) -> Result<DeploymentRecord, RevisionError> {
    validate_identifier(deployment_id, "deployment id")?;
    let path = root
        .join(STATE_DIR)
        .join(DEPLOYMENT_DIR)
        .join(format!("{deployment_id}.json"));
    let bytes = tokio::fs::read(path).await.map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            RevisionError::NotFound(deployment_id.to_owned())
        } else {
            RevisionError::Io(error)
        }
    })?;
    let record: DeploymentRecord = serde_json::from_slice(&bytes)
        .map_err(|error| RevisionError::InvalidRecord(error.to_string()))?;
    validate_hash(&record.content_hash)?;
    Ok(record)
}

pub async fn bind_run(
    root: &Path,
    deployment_id: &str,
    workflow_id: String,
    run_id: String,
) -> Result<DeploymentRecord, RevisionError> {
    let mut record = deployment(root, deployment_id).await?;
    record.workflow_id = Some(workflow_id);
    record.run_id = Some(run_id);
    record_deployment(root, &record).await?;
    Ok(record)
}

pub async fn current_drifted(
    root: &Path,
    record: &DeploymentRecord,
) -> Result<bool, RevisionError> {
    let current = super::documents::read(root, &record.document_path)
        .await
        .map_err(|error| RevisionError::InvalidRecord(error.to_string()))?;
    Ok(content_hash(&current.source) != record.content_hash)
}

async fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), RevisionError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| RevisionError::InvalidRecord(error.to_string()))?;
    let temp = temporary_path(path);
    tokio::fs::write(&temp, bytes).await?;
    if let Err(error) = tokio::fs::rename(&temp, path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(RevisionError::Io(error));
    }
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()))
}

fn validate_hash(hash: &str) -> Result<(), RevisionError> {
    if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(RevisionError::InvalidHash(hash.to_owned()))
    }
}

fn validate_identifier(value: &str, label: &str) -> Result<(), RevisionError> {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        Ok(())
    } else {
        Err(RevisionError::InvalidRecord(format!("invalid {label}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn revisions_are_content_addressed_and_immutable()
    -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let first = store(workspace.path(), "workflow first\n").await?;
        let same = store(workspace.path(), "workflow first\n").await?;
        let changed = store(workspace.path(), "workflow second\n").await?;
        assert_eq!(first.content_hash, same.content_hash);
        assert_ne!(first.content_hash, changed.content_hash);
        assert_eq!(
            fetch(workspace.path(), &first.content_hash).await?.source,
            first.source
        );
        assert_eq!(
            fetch(workspace.path(), &changed.content_hash).await?.source,
            changed.source
        );
        Ok(())
    }

    #[tokio::test]
    async fn deployment_round_trip_and_drift_detection() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        super::super::documents::write(
            workspace.path(),
            "flow.awl",
            super::super::documents::PutDocumentRequest {
                source: "workflow first\n".to_owned(),
            },
        )
        .await?;
        let revision = store(workspace.path(), "workflow first\n").await?;
        let record = DeploymentRecord {
            deployment_id: "deploy-1".to_owned(),
            document_path: "flow.awl".to_owned(),
            content_hash: revision.content_hash,
            package_id: "package-1".to_owned(),
            workflow_type: "flow".to_owned(),
            task_queue: "worker".to_owned(),
            workflow_id: None,
            run_id: None,
        };
        record_deployment(workspace.path(), &record).await?;
        assert_eq!(deployment(workspace.path(), "deploy-1").await?, record);
        assert!(!current_drifted(workspace.path(), &record).await?);
        super::super::documents::write(
            workspace.path(),
            "flow.awl",
            super::super::documents::PutDocumentRequest {
                source: "workflow second\n".to_owned(),
            },
        )
        .await?;
        assert!(current_drifted(workspace.path(), &record).await?);
        Ok(())
    }
}
