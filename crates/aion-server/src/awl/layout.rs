use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::filesystem::ConfinedDir;

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
    let document = validate_document_path(requested)?;
    let root = root.to_owned();
    let requested = requested.to_owned();
    let sidecar = sidecar_path(&document, subject);
    blocking("layout read", move || {
        let workspace = ConfinedDir::open(&root).map_err(LayoutError::Io)?;
        let source = read_document(&workspace, &document, &requested)?;
        let mut record = match workspace.read(&sidecar) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(invalid_json)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(LayoutRecord::default());
            }
            Err(error) => return Err(LayoutError::Io(error)),
        };
        let Some(names) = parsed_step_names(&source) else {
            return Ok(record);
        };
        let previous = record.positions.len();
        record.positions.retain(|name, _| names.contains(name));
        if record.positions.len() != previous {
            let bytes = serde_json::to_vec_pretty(&record).map_err(invalid_json)?;
            workspace.atomic_write(&sidecar, &bytes)?;
        }
        Ok(record)
    })
    .await
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
    let document = validate_document_path(requested)?;
    let root = root.to_owned();
    let requested = requested.to_owned();
    let sidecar = sidecar_path(&document, subject);
    blocking("layout write", move || {
        let workspace = ConfinedDir::open(&root).map_err(LayoutError::Io)?;
        let source = read_document(&workspace, &document, &requested)?;
        let names = parsed_step_names(&source).ok_or_else(|| {
            LayoutError::InvalidPath("cannot store layout for an invalid AWL document".to_owned())
        })?;
        if record.positions.keys().any(|name| !names.contains(name)) {
            return Err(LayoutError::InvalidPath(
                "layout contains a step name not declared by the document".to_owned(),
            ));
        }
        let bytes = serde_json::to_vec_pretty(&record).map_err(invalid_json)?;
        workspace.atomic_write(&sidecar, &bytes)?;
        Ok(record)
    })
    .await
}

fn validate_document_path(requested: &str) -> Result<PathBuf, LayoutError> {
    super::documents::document_path(requested).map_err(|error| {
        LayoutError::InvalidPath(error.to_string().replace("invalid AWL document path: ", ""))
    })
}

fn read_document(
    workspace: &ConfinedDir,
    relative: &Path,
    requested: &str,
) -> Result<String, LayoutError> {
    workspace.read_to_string(relative).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            LayoutError::DocumentNotFound(requested.to_owned())
        } else if matches!(
            error.kind(),
            io::ErrorKind::InvalidInput | io::ErrorKind::NotADirectory
        ) {
            LayoutError::InvalidPath(format!("document path contains a link: {error}"))
        } else {
            LayoutError::Io(error)
        }
    })
}

fn parsed_step_names(source: &str) -> Option<BTreeSet<String>> {
    aion_awl::parse(source)
        .ok()
        .map(|document| document.steps.into_iter().map(|step| step.name).collect())
}

fn sidecar_path(document: &Path, subject: &str) -> PathBuf {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut user = String::with_capacity(subject.len() * 2);
    for byte in subject.as_bytes() {
        user.push(char::from(HEX[usize::from(byte >> 4)]));
        user.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    let mut path = Path::new(".aion").join("layout").join(user).join(document);
    let file_name = path
        .file_name()
        .unwrap_or(OsStr::new("document.awl"))
        .to_string_lossy();
    path.set_file_name(format!("{file_name}.json"));
    path
}

async fn blocking<T: Send + 'static>(
    operation: &'static str,
    work: impl FnOnce() -> Result<T, LayoutError> + Send + 'static,
) -> Result<T, LayoutError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| io::Error::other(format!("{operation} task failed: {error}")))?
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
        super::super::documents::write(
            workspace.path(),
            "flow.awl",
            super::super::documents::PutDocumentRequest {
                source: SOURCE.to_owned(),
            },
        )
        .await?;
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
        Ok(())
    }

    #[tokio::test]
    async fn invalid_positions_and_unknown_steps_are_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        super::super::documents::write(
            workspace.path(),
            "flow.awl",
            super::super::documents::PutDocumentRequest {
                source: SOURCE.to_owned(),
            },
        )
        .await?;
        let invalid = LayoutRecord {
            positions: BTreeMap::from([(
                "first".to_owned(),
                LayoutPosition {
                    x: f64::NAN,
                    y: 0.0,
                },
            )]),
        };
        assert!(matches!(
            write(workspace.path(), "flow.awl", "alice", invalid).await,
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

    #[cfg(unix)]
    #[tokio::test]
    async fn layout_parent_and_predictable_temp_links_cannot_escape()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let sandbox = tempfile::tempdir()?;
        let workspace = sandbox.path().join("workspace");
        let outside = sandbox.path().join("outside");
        std::fs::create_dir(&workspace)?;
        std::fs::create_dir(&outside)?;
        super::super::documents::write(
            &workspace,
            "flow.awl",
            super::super::documents::PutDocumentRequest {
                source: SOURCE.to_owned(),
            },
        )
        .await?;
        symlink(&outside, workspace.join(".aion"))?;
        let record = LayoutRecord {
            positions: BTreeMap::from([("first".to_owned(), LayoutPosition { x: 1.0, y: 2.0 })]),
        };
        assert!(
            write(&workspace, "flow.awl", "alice", record)
                .await
                .is_err()
        );
        assert!(std::fs::read_dir(&outside)?.next().is_none());

        std::fs::remove_file(workspace.join(".aion"))?;
        std::fs::create_dir(workspace.join(".aion"))?;
        let victim = outside.join("victim");
        symlink(&victim, workspace.join(".aion/.aion-layout.tmp"))?;
        let record = LayoutRecord {
            positions: BTreeMap::from([("first".to_owned(), LayoutPosition { x: 1.0, y: 2.0 })]),
        };
        write(&workspace, "flow.awl", "alice", record).await?;
        assert!(!victim.exists());
        Ok(())
    }
}
