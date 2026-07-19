use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::filesystem::ConfinedDir;

#[derive(Debug, Serialize)]
pub struct DocumentEntry {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct DocumentResponse {
    pub source: String,
    pub content_hash: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateDocumentRequest {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct CreateDocumentResponse {
    pub path: String,
    pub name: String,
    pub source: String,
    pub content_hash: String,
}

#[derive(Debug, Deserialize)]
pub struct PutDocumentRequest {
    pub source: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    #[error("invalid AWL document path: {0}")]
    InvalidPath(String),
    #[error("invalid AWL document name: {0}")]
    InvalidName(String),
    #[error("AWL document was not found: {0}")]
    NotFound(String),
    #[error("AWL document already exists: {0}")]
    Exists(String),
    #[error("AWL workspace is not configured")]
    WorkspaceUnconfigured,
    #[error("AWL workspace I/O failed: {0}")]
    Io(#[from] io::Error),
}

pub async fn list(root: &Path) -> Result<Vec<DocumentEntry>, DocumentError> {
    let root = root.to_owned();
    blocking("document listing", move || {
        let workspace = match ConfinedDir::open(&root) {
            Ok(workspace) => workspace,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(DocumentError::Io(error)),
        };
        let mut entries: Vec<_> = workspace
            .list_awl()?
            .into_iter()
            .map(|path| DocumentEntry {
                name: path
                    .file_stem()
                    .unwrap_or(OsStr::new(""))
                    .to_string_lossy()
                    .into_owned(),
                path: path.to_string_lossy().replace('\\', "/"),
            })
            .collect();
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(entries)
    })
    .await
}

pub async fn read(root: &Path, requested: &str) -> Result<DocumentResponse, DocumentError> {
    let relative = document_path(requested)?;
    let root = root.to_owned();
    let requested = requested.to_owned();
    let source = blocking("document read", move || {
        let workspace = ConfinedDir::open(&root).map_err(DocumentError::Io)?;
        workspace.read_to_string(&relative).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                DocumentError::NotFound(requested)
            } else {
                confinement_error(error)
            }
        })
    })
    .await?;
    Ok(DocumentResponse {
        content_hash: super::revisions::content_hash(&source),
        source,
    })
}

pub async fn create(
    root: &Path,
    request: CreateDocumentRequest,
) -> Result<CreateDocumentResponse, DocumentError> {
    validate_document_name(&request.name)?;
    let source = new_document_source(&request.name)?;
    let path = format!("{}.awl", request.name);
    let root_owned = root.to_owned();
    let path_owned = PathBuf::from(&path);
    let source_owned = source.clone();
    blocking("document create", move || {
        let workspace = ConfinedDir::open_or_create(&root_owned).map_err(confinement_error)?;
        workspace
            .create_new(&path_owned, source_owned.as_bytes())
            .map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    DocumentError::Exists(path_owned.to_string_lossy().into_owned())
                } else {
                    confinement_error(error)
                }
            })
    })
    .await?;
    let revision = match super::revisions::store(root, &source).await {
        Ok(revision) => revision,
        Err(error) => {
            if let Ok(workspace) = ConfinedDir::open(root) {
                let _ = workspace.remove_file(Path::new(&path));
            }
            return Err(revision_io(&error));
        }
    };
    Ok(CreateDocumentResponse {
        path,
        name: request.name,
        source,
        content_hash: revision.content_hash,
    })
}

pub async fn write(
    root: &Path,
    requested: &str,
    request: PutDocumentRequest,
) -> Result<DocumentResponse, DocumentError> {
    let relative = document_path(requested)?;
    let root_owned = root.to_owned();
    let source_owned = request.source.clone();
    blocking("document write", move || {
        let workspace = ConfinedDir::open_or_create(&root_owned).map_err(confinement_error)?;
        workspace
            .atomic_write(&relative, source_owned.as_bytes())
            .map_err(confinement_error)
    })
    .await?;
    let revision = super::revisions::store(root, &request.source)
        .await
        .map_err(|error| revision_io(&error))?;
    Ok(DocumentResponse {
        source: request.source,
        content_hash: revision.content_hash,
    })
}

pub(crate) fn document_path(requested: &str) -> Result<PathBuf, DocumentError> {
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
    if path.extension() != Some(OsStr::new("awl")) {
        return Err(DocumentError::InvalidPath(
            "document path must end in `.awl`".to_owned(),
        ));
    }
    Ok(path.to_owned())
}

fn confinement_error(error: io::Error) -> DocumentError {
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::NotADirectory
    ) {
        DocumentError::InvalidPath(format!(
            "workspace paths must contain only real directories and files: {error}"
        ))
    } else {
        DocumentError::Io(error)
    }
}

async fn blocking<T: Send + 'static>(
    operation: &'static str,
    work: impl FnOnce() -> Result<T, DocumentError> + Send + 'static,
) -> Result<T, DocumentError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| io::Error::other(format!("{operation} task failed: {error}")))?
}

fn revision_io(error: &super::revisions::RevisionError) -> DocumentError {
    DocumentError::Io(io::Error::other(error.to_string()))
}

fn validate_document_name(name: &str) -> Result<(), DocumentError> {
    let mut characters = name.chars();
    let starts_validly = characters
        .next()
        .is_some_and(|character| character.is_ascii_lowercase() || character == '_');
    if !starts_validly
        || !characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
    {
        return Err(DocumentError::InvalidName(
            "use a lowercase AWL identifier (letters, digits, and underscores)".to_owned(),
        ));
    }
    Ok(())
}

fn new_document_source(name: &str) -> Result<String, DocumentError> {
    let source = format!(
        "//! {name} workflow.\nworkflow {name}\n  outcome done: type Placeholder, route success\n\ntype Placeholder {{ value: String }}\n"
    );
    let document =
        aion_awl::parse(&source).map_err(|error| DocumentError::InvalidName(error.message))?;
    Ok(aion_awl::print(&document))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_workspace_lists_empty_without_materializing_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let workspace = parent.path().join("aion-authoring");
        assert!(list(&workspace).await?.is_empty());
        assert!(!workspace.exists());
        Ok(())
    }

    #[tokio::test]
    async fn workspace_round_trip_rejects_traversal() -> Result<(), Box<dyn std::error::Error>> {
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
        assert_eq!(
            read(workspace.path(), "nested/example.awl").await?.source,
            "workflow example\n"
        );
        assert!(matches!(
            write(
                workspace.path(),
                "../outside.awl",
                PutDocumentRequest {
                    source: String::new()
                }
            )
            .await,
            Err(DocumentError::InvalidPath(_))
        ));
        assert!(matches!(
            read(workspace.path(), "/tmp/outside.awl").await,
            Err(DocumentError::InvalidPath(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn create_is_atomic_typed_and_private() -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let created = create(
            workspace.path(),
            CreateDocumentRequest {
                name: "first_workflow".to_owned(),
            },
        )
        .await?;
        assert_eq!(created.path, "first_workflow.awl");
        assert_eq!(
            aion_awl::print(&aion_awl::parse(&created.source)?),
            created.source
        );
        assert!(matches!(
            create(
                workspace.path(),
                CreateDocumentRequest {
                    name: "first_workflow".to_owned()
                }
            )
            .await,
            Err(DocumentError::Exists(_))
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(workspace.path().join(&created.path))?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn root_parent_and_dangling_temp_links_cannot_escape()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let sandbox = tempfile::tempdir()?;
        let outside = sandbox.path().join("outside");
        std::fs::create_dir(&outside)?;
        let root_link = sandbox.path().join("root-link");
        symlink(&outside, &root_link)?;
        assert!(
            write(
                &root_link,
                "escape.awl",
                PutDocumentRequest {
                    source: "escaped".to_owned()
                }
            )
            .await
            .is_err()
        );
        assert!(!outside.join("escape.awl").exists());

        let workspace = sandbox.path().join("workspace");
        std::fs::create_dir(&workspace)?;
        symlink(&outside, workspace.join("linked"))?;
        assert!(
            write(
                &workspace,
                "linked/escape.awl",
                PutDocumentRequest {
                    source: "escaped".to_owned()
                }
            )
            .await
            .is_err()
        );
        assert!(!outside.join("escape.awl").exists());

        let victim = outside.join("victim");
        symlink(&victim, workspace.join(".victim.awl.aion-awl.tmp"))?;
        write(
            &workspace,
            "victim.awl",
            PutDocumentRequest {
                source: "safe".to_owned(),
            },
        )
        .await?;
        assert!(
            !victim.exists(),
            "predictable dangling temp link was followed"
        );
        Ok(())
    }
}
