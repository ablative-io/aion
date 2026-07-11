use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct DocumentEntry {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct DocumentResponse {
    pub source: String,
}

#[derive(Debug, Deserialize)]
pub struct PutDocumentRequest {
    pub source: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    #[error("invalid AWL document path: {0}")]
    InvalidPath(String),
    #[error("AWL document was not found: {0}")]
    NotFound(String),
    #[error("AWL workspace I/O failed: {0}")]
    Io(#[from] io::Error),
}

pub async fn list(root: &Path) -> Result<Vec<DocumentEntry>, DocumentError> {
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || list_sync(&root))
        .await
        .map_err(|error| io::Error::other(format!("document listing task failed: {error}")))?
}

pub async fn read(root: &Path, requested: &str) -> Result<DocumentResponse, DocumentError> {
    let path = resolve_existing(root, requested)?;
    let source = tokio::fs::read_to_string(&path).await.map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            DocumentError::NotFound(requested.to_owned())
        } else {
            DocumentError::Io(error)
        }
    })?;
    Ok(DocumentResponse { source })
}

pub async fn write(
    root: &Path,
    requested: &str,
    request: PutDocumentRequest,
) -> Result<DocumentResponse, DocumentError> {
    let relative = validate_relative(requested)?;
    ensure_awl(&relative)?;
    tokio::fs::create_dir_all(root).await?;
    let canonical_root = tokio::fs::canonicalize(root).await?;
    let parent_relative = relative.parent().unwrap_or_else(|| Path::new(""));
    create_safe_parents(&canonical_root, parent_relative).await?;
    let destination = canonical_root.join(&relative);
    if let Ok(metadata) = tokio::fs::symlink_metadata(&destination).await {
        if metadata.file_type().is_symlink() {
            return Err(DocumentError::InvalidPath(
                "symbolic links are not writable workspace documents".to_owned(),
            ));
        }
    }
    let parent = destination.parent().ok_or_else(|| {
        DocumentError::InvalidPath("document path has no parent directory".to_owned())
    })?;
    let mut temp = tempfile_path(
        parent,
        destination.file_name().unwrap_or(OsStr::new("document")),
    );
    let mut suffix = 0_u32;
    while tokio::fs::try_exists(&temp).await? {
        suffix += 1;
        temp = parent.join(format!(".aion-awl-{suffix}.tmp"));
    }
    tokio::fs::write(&temp, request.source.as_bytes()).await?;
    if let Err(error) = tokio::fs::rename(&temp, &destination).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(DocumentError::Io(error));
    }
    Ok(DocumentResponse {
        source: request.source,
    })
}

fn list_sync(root: &Path) -> Result<Vec<DocumentEntry>, DocumentError> {
    let canonical_root = std::fs::canonicalize(root)?;
    let mut entries = Vec::new();
    visit(&canonical_root, &canonical_root, &mut entries)?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

fn visit(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<DocumentEntry>,
) -> Result<(), DocumentError> {
    for item in std::fs::read_dir(directory)? {
        let item = item?;
        let file_type = item.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = item.path();
        if file_type.is_dir() {
            visit(root, &path, entries)?;
        } else if file_type.is_file() && path.extension() == Some(OsStr::new("awl")) {
            let relative = path.strip_prefix(root).map_err(|error| {
                DocumentError::InvalidPath(format!("workspace path escaped its root: {error}"))
            })?;
            let path_text = relative.to_string_lossy().replace('\\', "/");
            let name = path
                .file_stem()
                .unwrap_or(OsStr::new(""))
                .to_string_lossy()
                .into_owned();
            entries.push(DocumentEntry {
                path: path_text,
                name,
            });
        }
    }
    Ok(())
}

fn resolve_existing(root: &Path, requested: &str) -> Result<PathBuf, DocumentError> {
    let relative = validate_relative(requested)?;
    ensure_awl(&relative)?;
    let canonical_root = std::fs::canonicalize(root)?;
    let candidate = canonical_root.join(relative);
    let canonical = std::fs::canonicalize(&candidate).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            DocumentError::NotFound(requested.to_owned())
        } else {
            DocumentError::Io(error)
        }
    })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(DocumentError::InvalidPath(
            "path resolves outside the workspace".to_owned(),
        ));
    }
    Ok(canonical)
}

fn validate_relative(requested: &str) -> Result<PathBuf, DocumentError> {
    let path = Path::new(requested);
    if requested.is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(DocumentError::InvalidPath(
            "path must be non-empty, relative, and contain no `..` components".to_owned(),
        ));
    }
    Ok(path.to_owned())
}

fn ensure_awl(path: &Path) -> Result<(), DocumentError> {
    if path.extension() != Some(OsStr::new("awl")) {
        return Err(DocumentError::InvalidPath(
            "document path must end in `.awl`".to_owned(),
        ));
    }
    Ok(())
}

async fn create_safe_parents(root: &Path, relative: &Path) -> Result<(), DocumentError> {
    let mut current = root.to_owned();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(DocumentError::InvalidPath("invalid parent path".to_owned()));
        };
        current.push(name);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(DocumentError::InvalidPath(
                    "workspace parent is not a real directory".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                tokio::fs::create_dir(&current).await?;
            }
            Err(error) => return Err(DocumentError::Io(error)),
        }
    }
    Ok(())
}

fn tempfile_path(parent: &Path, name: &OsStr) -> PathBuf {
    parent.join(format!(".{}.aion-awl.tmp", name.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn workspace_list_read_write_round_trip_and_rejects_traversal()
    -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        write(
            workspace.path(),
            "nested/example.awl",
            PutDocumentRequest {
                source: "workflow example\n".to_owned(),
            },
        )
        .await?;

        let entries = list(workspace.path()).await?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "nested/example.awl");
        assert_eq!(entries[0].name, "example");
        let loaded = read(workspace.path(), "nested/example.awl").await?;
        assert_eq!(loaded.source, "workflow example\n");

        let refusal = write(
            workspace.path(),
            "../outside.awl",
            PutDocumentRequest {
                source: String::new(),
            },
        )
        .await;
        assert!(matches!(refusal, Err(DocumentError::InvalidPath(_))));
        let absolute = read(workspace.path(), "/tmp/outside.awl").await;
        assert!(matches!(absolute, Err(DocumentError::InvalidPath(_))));
        Ok(())
    }
}
