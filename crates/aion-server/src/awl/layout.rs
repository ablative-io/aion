use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
pub struct LayoutPosition {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct LayoutRecord {
    pub positions: BTreeMap<String, LayoutPosition>,
}

#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("invalid AWL layout path: {0}")]
    InvalidPath(String),
    #[error("AWL document was not found: {0}")]
    DocumentNotFound(String),
    #[error("AWL layout I/O failed: {0}")]
    Io(#[from] io::Error),
}

pub async fn read(
    root: &Path,
    requested: &str,
    subject: &str,
) -> Result<LayoutRecord, LayoutError> {
    let relative = validate_document_path(requested)?;
    let canonical_root = canonical_workspace(root).await?;
    let source = read_document(&canonical_root, &relative, requested).await?;
    let sidecar = sidecar_path(&canonical_root, &relative, subject);
    if !safe_existing_path(&canonical_root, &sidecar).await? {
        return Ok(LayoutRecord::default());
    }
    let mut record = match tokio::fs::read(&sidecar).await {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(invalid_json)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(LayoutRecord::default()),
        Err(error) => return Err(LayoutError::Io(error)),
    };
    let Some(names) = parsed_step_names(&source) else {
        return Ok(record);
    };
    let previous = record.positions.len();
    record.positions.retain(|name, _| names.contains(name));
    if record.positions.len() != previous {
        atomic_write(&canonical_root, &sidecar, &record).await?;
    }
    Ok(record)
}

pub async fn write(
    root: &Path,
    requested: &str,
    subject: &str,
    record: LayoutRecord,
) -> Result<LayoutRecord, LayoutError> {
    if record
        .positions
        .values()
        .any(|position| !position.x.is_finite() || !position.y.is_finite())
    {
        return Err(LayoutError::InvalidPath(
            "layout positions must be finite numbers".to_owned(),
        ));
    }
    let relative = validate_document_path(requested)?;
    let canonical_root = canonical_workspace(root).await?;
    let source = read_document(&canonical_root, &relative, requested).await?;
    let names = parsed_step_names(&source).ok_or_else(|| {
        LayoutError::InvalidPath("cannot store layout for an invalid AWL document".to_owned())
    })?;
    if record.positions.keys().any(|name| !names.contains(name)) {
        return Err(LayoutError::InvalidPath(
            "layout contains a step name not declared by the document".to_owned(),
        ));
    }
    let sidecar = sidecar_path(&canonical_root, &relative, subject);
    atomic_write(&canonical_root, &sidecar, &record).await?;
    Ok(record)
}

fn validate_document_path(requested: &str) -> Result<PathBuf, LayoutError> {
    let path = Path::new(requested);
    if requested.is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || path.extension() != Some(OsStr::new("awl"))
    {
        return Err(LayoutError::InvalidPath(
            "path must be relative, contain no `..`, and end in `.awl`".to_owned(),
        ));
    }
    Ok(path.to_owned())
}

async fn canonical_workspace(root: &Path) -> Result<PathBuf, LayoutError> {
    tokio::fs::canonicalize(root).await.map_err(LayoutError::Io)
}

async fn read_document(
    root: &Path,
    relative: &Path,
    requested: &str,
) -> Result<String, LayoutError> {
    let candidate = root.join(relative);
    let canonical = tokio::fs::canonicalize(&candidate).await.map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            LayoutError::DocumentNotFound(requested.to_owned())
        } else {
            LayoutError::Io(error)
        }
    })?;
    if !canonical.starts_with(root) {
        return Err(LayoutError::InvalidPath(
            "document path resolves outside the workspace".to_owned(),
        ));
    }
    tokio::fs::read_to_string(canonical)
        .await
        .map_err(LayoutError::Io)
}

fn parsed_step_names(source: &str) -> Option<BTreeSet<String>> {
    aion_awl::parse(source)
        .ok()
        .map(|document| document.steps.into_iter().map(|step| step.name).collect())
}

fn sidecar_path(root: &Path, document: &Path, subject: &str) -> PathBuf {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut user = String::with_capacity(subject.len() * 2);
    for byte in subject.as_bytes() {
        user.push(char::from(HEX[usize::from(byte >> 4)]));
        user.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    let mut path = root.join(".aion").join("layout").join(user).join(document);
    let file_name = path
        .file_name()
        .unwrap_or(OsStr::new("document.awl"))
        .to_string_lossy();
    path.set_file_name(format!("{file_name}.json"));
    path
}

async fn atomic_write(
    root: &Path,
    destination: &Path,
    record: &LayoutRecord,
) -> Result<(), LayoutError> {
    let parent = destination.parent().ok_or_else(|| {
        LayoutError::InvalidPath("layout path has no parent directory".to_owned())
    })?;
    create_safe_directories(root, parent).await?;
    if let Ok(metadata) = tokio::fs::symlink_metadata(destination).await {
        if metadata.file_type().is_symlink() {
            return Err(LayoutError::InvalidPath(
                "symbolic links are not writable layout records".to_owned(),
            ));
        }
    }
    let bytes = serde_json::to_vec_pretty(record).map_err(invalid_json)?;
    let temp = parent.join(".aion-layout.tmp");
    tokio::fs::write(&temp, bytes).await?;
    if let Err(error) = tokio::fs::rename(&temp, destination).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(LayoutError::Io(error));
    }
    Ok(())
}

async fn safe_existing_path(root: &Path, destination: &Path) -> Result<bool, LayoutError> {
    let relative = destination
        .strip_prefix(root)
        .map_err(|_| LayoutError::InvalidPath("layout path escaped the workspace".to_owned()))?;
    let mut current = root.to_owned();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(LayoutError::InvalidPath("invalid layout path".to_owned()));
        };
        current.push(name);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LayoutError::InvalidPath(
                    "symbolic links are not readable layout records".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(LayoutError::Io(error)),
        }
    }
    Ok(true)
}

async fn create_safe_directories(root: &Path, destination: &Path) -> Result<(), LayoutError> {
    let relative = destination
        .strip_prefix(root)
        .map_err(|_| LayoutError::InvalidPath("layout path escaped the workspace".to_owned()))?;
    let mut current = root.to_owned();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(LayoutError::InvalidPath("invalid layout path".to_owned()));
        };
        current.push(name);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(LayoutError::InvalidPath(
                    "layout parent is not a real directory".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                tokio::fs::create_dir(&current).await?;
            }
            Err(error) => return Err(LayoutError::Io(error)),
        }
    }
    Ok(())
}

fn invalid_json(error: impl std::fmt::Display) -> LayoutError {
    LayoutError::Io(io::Error::new(
        io::ErrorKind::InvalidData,
        error.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "//! Layout test\nworkflow layout_test\n  outcome done: type String, route success\n\n/// First step\nstep first\n  route done(value: \"ok\")\n";

    #[tokio::test]
    async fn layout_is_per_user_atomic_and_garbage_collects_orphans()
    -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        tokio::fs::write(workspace.path().join("flow.awl"), SOURCE).await?;
        let alice = LayoutRecord {
            positions: BTreeMap::from([("first".to_owned(), LayoutPosition { x: 10.0, y: 20.0 })]),
        };
        write(workspace.path(), "flow.awl", "alice", alice).await?;
        let stored_x = read(workspace.path(), "flow.awl", "alice").await?.positions["first"].x;
        assert!((stored_x - 10.0).abs() < f64::EPSILON);
        assert!(
            read(workspace.path(), "flow.awl", "bob")
                .await?
                .positions
                .is_empty()
        );

        let sidecar = sidecar_path(workspace.path(), Path::new("flow.awl"), "alice");
        let mut stored: LayoutRecord = serde_json::from_slice(&tokio::fs::read(&sidecar).await?)?;
        stored
            .positions
            .insert("deleted".to_owned(), LayoutPosition { x: 1.0, y: 2.0 });
        tokio::fs::write(&sidecar, serde_json::to_vec(&stored)?).await?;
        let cleaned = read(workspace.path(), "flow.awl", "alice").await?;
        assert_eq!(cleaned.positions.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn layout_refuses_traversal_and_unknown_step_names()
    -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        tokio::fs::write(workspace.path().join("flow.awl"), SOURCE).await?;
        assert!(matches!(
            read(workspace.path(), "../flow.awl", "alice").await,
            Err(LayoutError::InvalidPath(_))
        ));
        let unknown = LayoutRecord {
            positions: BTreeMap::from([("missing".to_owned(), LayoutPosition { x: 0.0, y: 0.0 })]),
        };
        assert!(matches!(
            write(workspace.path(), "flow.awl", "alice", unknown).await,
            Err(LayoutError::InvalidPath(_))
        ));
        Ok(())
    }
}
