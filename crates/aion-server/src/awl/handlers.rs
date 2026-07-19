use std::io;
use std::path::{Component, Path};

use aion_awl::{Span, TypeBody, parse, print, semantic};
use serde::{Deserialize, Serialize};

use crate::filesystem::ConfinedDir;

#[derive(Debug, Deserialize)]
pub struct CheckRequest {
    pub source: String,
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CheckResponse {
    pub ok: bool,
    pub deploys_green: bool,
    pub steps: Option<usize>,
    pub diagnostics: Vec<Diagnostic>,
    pub semantic: Option<SemanticIndex>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub class: DiagnosticClass,
    pub message: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticClass {
    Error,
}

#[derive(Debug, Deserialize)]
pub struct FormatRequest {
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct FormatResponse {
    pub formatted: String,
}

#[derive(Debug, Serialize)]
pub struct SemanticIndex {
    pub entries: Vec<SemanticEntry>,
    pub graph: super::projection::GraphProjection,
    pub studio: super::studio_projection::StudioProjection,
}

#[derive(Debug, Serialize)]
pub struct SemanticEntry {
    pub span: SourceSpan,
    #[serde(rename = "type")]
    pub type_text: Option<String>,
    pub declaration: Option<SemanticDeclaration>,
}

#[derive(Debug, Serialize)]
pub struct SemanticDeclaration {
    pub name: String,
    pub kind: String,
    pub documentation: Option<String>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SourceSpan {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
}

pub fn check_source(request: &CheckRequest) -> CheckResponse {
    check_source_at(request, None)
}

fn check_source_at(request: &CheckRequest, root: Option<&Path>) -> CheckResponse {
    let document = match parse(&request.source) {
        Ok(document) => document,
        Err(error) => {
            return CheckResponse {
                ok: false,
                deploys_green: false,
                steps: None,
                diagnostics: vec![diagnostic(
                    DiagnosticClass::Error,
                    error.message,
                    error.span,
                )],
                semantic: None,
            };
        }
    };
    let analysis = root.map_or_else(
        || semantic::analyze(&document),
        |root| semantic::analyze_in(&document, root),
    );
    let diagnostics: Vec<_> = analysis
        .diagnostics()
        .iter()
        .map(|error| diagnostic(DiagnosticClass::Error, error.message.clone(), error.span))
        .collect();
    if !diagnostics.is_empty() {
        return CheckResponse {
            ok: false,
            deploys_green: false,
            steps: None,
            diagnostics,
            semantic: None,
        };
    }
    let graph = super::projection::build(&document, analysis.step_kinds());
    let studio = super::studio_projection::build(&document);
    let semantic = SemanticIndex {
        entries: analysis
            .iter()
            .map(|info| SemanticEntry {
                span: info.span.into(),
                type_text: info.ty.clone(),
                declaration: info
                    .declaration
                    .as_ref()
                    .map(|declaration| SemanticDeclaration {
                        name: declaration.name.clone(),
                        kind: declaration.kind.as_str().to_owned(),
                        documentation: declaration.documentation.clone(),
                        span: declaration.span.into(),
                    }),
            })
            .collect(),
        graph,
        studio,
    };
    CheckResponse {
        ok: true,
        deploys_green: diagnostics.is_empty(),
        steps: Some(document.steps.len()),
        diagnostics,
        semantic: Some(semantic),
    }
}

pub async fn check_source_in_workspace(
    workspace_root: &Path,
    request: &CheckRequest,
) -> Result<CheckResponse, super::documents::DocumentError> {
    let Some(requested_path) = request.path.as_deref() else {
        return Ok(check_source(request));
    };
    let document_path = super::documents::document_path(requested_path)?;
    let workspace_root = workspace_root.to_owned();
    let source = request.source.clone();
    let requested_path = requested_path.to_owned();
    tokio::task::spawn_blocking(move || {
        let Ok(document) = parse(&source) else {
            return Ok(check_source(&CheckRequest {
                source,
                path: Some(requested_path),
            }));
        };
        let workspace =
            ConfinedDir::open(&workspace_root).map_err(super::documents::DocumentError::Io)?;
        let staging = tempfile::Builder::new().prefix("aion-schema-").tempdir()?;
        let document_parent = document_path.parent().unwrap_or_else(|| Path::new(""));
        let analysis_root = staging.path().join(document_parent);
        std::fs::create_dir_all(&analysis_root)?;
        for declaration in &document.types {
            let TypeBody::SchemaImport { path, .. } = &declaration.body else {
                continue;
            };
            let import = Path::new(path);
            if path.is_empty()
                || import
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
            {
                continue;
            }
            let workspace_path = document_parent.join(import);
            let bytes = workspace.read(&workspace_path).map_err(|error| {
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::NotADirectory
                ) {
                    super::documents::DocumentError::InvalidPath(format!(
                        "schema import `{path}` contains a link: {error}"
                    ))
                } else {
                    super::documents::DocumentError::Io(error)
                }
            })?;
            let staged = analysis_root.join(import);
            if let Some(parent) = staged.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(staged, bytes)?;
        }
        Ok(check_source_at(
            &CheckRequest {
                source,
                path: Some(requested_path),
            },
            Some(&analysis_root),
        ))
    })
    .await
    .map_err(|error| {
        super::documents::DocumentError::Io(io::Error::other(format!(
            "AWL check task failed: {error}"
        )))
    })?
}

pub(crate) fn stage_schema_imports(
    workspace_root: &Path,
    requested_path: &str,
    source: &str,
) -> Result<(tempfile::TempDir, std::path::PathBuf), super::documents::DocumentError> {
    let document_path = super::documents::document_path(requested_path)?;
    let document = parse(source)
        .map_err(|error| super::documents::DocumentError::InvalidPath(error.message))?;
    let workspace =
        ConfinedDir::open(workspace_root).map_err(super::documents::DocumentError::Io)?;
    let staging = tempfile::Builder::new().prefix("aion-schema-").tempdir()?;
    let document_parent = document_path.parent().unwrap_or_else(|| Path::new(""));
    let analysis_root = staging.path().join(document_parent);
    std::fs::create_dir_all(&analysis_root)?;
    for declaration in &document.types {
        let TypeBody::SchemaImport { path, .. } = &declaration.body else {
            continue;
        };
        let import = Path::new(path);
        if path.is_empty()
            || import
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            continue;
        }
        let bytes = workspace
            .read(&document_parent.join(import))
            .map_err(|error| {
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::NotADirectory
                ) {
                    super::documents::DocumentError::InvalidPath(format!(
                        "schema import `{path}` contains a link: {error}"
                    ))
                } else {
                    super::documents::DocumentError::Io(error)
                }
            })?;
        let staged = analysis_root.join(import);
        if let Some(parent) = staged.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(staged, bytes)?;
    }
    Ok((staging, analysis_root))
}

pub fn format_source(request: &FormatRequest) -> Result<FormatResponse, Diagnostic> {
    parse(&request.source)
        .map(|document| FormatResponse {
            formatted: print(&document),
        })
        .map_err(|error| diagnostic(DiagnosticClass::Error, error.message, error.span))
}

fn diagnostic(class: DiagnosticClass, message: String, span: Span) -> Diagnostic {
    Diagnostic {
        class,
        message,
        line: span.line,
        column: span.column,
    }
}

impl From<Span> for SourceSpan {
    fn from(span: Span) -> Self {
        Self {
            start: span.start,
            end: span.end,
            line: span.line,
            column: span.column,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HAPPY: &str =
        include_str!("../../../aion-awl/tests/fixtures/rev2/dag-fork/valid/after_single.awl");
    const INVALID: &str = include_str!(
        "../../../aion-awl/tests/fixtures/rev2/dag-fork/invalid/unknown_after_target.awl"
    );
    const EMIT_REFUSED: &str = "//! Emitter refusal probe.\nworkflow emit_refused\n  outcome done: type Result, route success\n\ntype Result { value: String }\n\nworker work\n  action make() -> Result\n\nstep finish\n  make() -> result\n  route done(value: result.value)\n  on failure\n    route done(value: \"failed\")\n";

    #[test]
    fn check_happy_path_returns_steps_and_semantics() {
        let response = check_source(&CheckRequest {
            source: HAPPY.to_owned(),
            path: None,
        });
        assert!(response.ok);
        assert!(response.deploys_green);
        assert_eq!(response.steps, Some(2));
        assert!(response.diagnostics.is_empty());
        assert!(
            response
                .semantic
                .is_some_and(|semantic| !semantic.entries.is_empty())
        );
    }

    #[test]
    fn checker_message_surfaces_verbatim() -> Result<(), Box<dyn std::error::Error>> {
        let document = parse(INVALID)?;
        let expected = aion_awl::check(&document)
            .first()
            .ok_or("fixture unexpectedly checks cleanly")?
            .message
            .clone();
        let response = check_source(&CheckRequest {
            source: INVALID.to_owned(),
            path: None,
        });
        assert!(!response.ok);
        assert_eq!(response.diagnostics[0].message, expected);
        assert!(matches!(
            response.diagnostics[0].class,
            DiagnosticClass::Error
        ));
        Ok(())
    }

    #[test]
    fn checker_only_check_does_not_consult_the_legacy_emitter() {
        let response = check_source(&CheckRequest {
            source: EMIT_REFUSED.to_owned(),
            path: None,
        });
        assert!(response.ok, "diagnostics: {:?}", response.diagnostics);
        assert!(response.deploys_green);
        assert!(response.diagnostics.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn http_check_confines_document_paths_and_schema_imports()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir()?;
        let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aion-awl/tests/fixtures/rev2/schema-doors/valid");
        let source = std::fs::read_to_string(fixture_dir.join("mixed_doors.awl"))?;
        let schema = std::fs::read(fixture_dir.join("intake.schema.json"))?;
        std::fs::create_dir(workspace.path().join("nested"))?;
        std::fs::write(workspace.path().join("nested/intake.schema.json"), schema)?;

        let valid = check_source_in_workspace(
            workspace.path(),
            &CheckRequest {
                source: source.clone(),
                path: Some("nested/mixed_doors.awl".to_owned()),
            },
        )
        .await?;
        assert!(valid.ok, "confined import failed: {:?}", valid.diagnostics);

        let outside = workspace.path().parent().ok_or("workspace had no parent")?;
        let absolute_source = source.replace(
            "intake.schema.json",
            &outside.join("outside.schema.json").to_string_lossy(),
        );
        let absolute = check_source_in_workspace(
            workspace.path(),
            &CheckRequest {
                source: absolute_source,
                path: Some("nested/mixed_doors.awl".to_owned()),
            },
        )
        .await?;
        assert!(!absolute.ok);
        assert!(absolute.diagnostics[0].message.contains("relative path"));

        let traversal = check_source_in_workspace(
            workspace.path(),
            &CheckRequest {
                source: source.replace("intake.schema.json", "../outside.schema.json"),
                path: Some("nested/mixed_doors.awl".to_owned()),
            },
        )
        .await?;
        assert!(!traversal.ok);
        assert!(traversal.diagnostics[0].message.contains("no `..`"));

        let absolute_document = check_source_in_workspace(
            workspace.path(),
            &CheckRequest {
                source: source.clone(),
                path: Some("/tmp/mixed_doors.awl".to_owned()),
            },
        )
        .await;
        assert!(matches!(
            absolute_document,
            Err(super::super::documents::DocumentError::InvalidPath(_))
        ));

        let external_schema = outside.join("external.schema.json");
        std::fs::write(&external_schema, b"{\"type\":\"object\"}")?;
        symlink(
            &external_schema,
            workspace.path().join("nested/linked.schema.json"),
        )?;
        let linked = check_source_in_workspace(
            workspace.path(),
            &CheckRequest {
                source: source.replace("intake.schema.json", "linked.schema.json"),
                path: Some("nested/mixed_doors.awl".to_owned()),
            },
        )
        .await;
        assert!(linked.is_err(), "schema symlink was followed");
        Ok(())
    }

    #[test]
    fn format_is_canonical_and_idempotent() -> Result<(), Diagnostic> {
        let once = format_source(&FormatRequest {
            source: HAPPY.replace("type Summary  {", "type Summary {"),
        })?;
        let twice = format_source(&FormatRequest {
            source: once.formatted.clone(),
        })?;
        assert_eq!(once.formatted, twice.formatted);
        assert_eq!(once.formatted, HAPPY);
        Ok(())
    }

    #[test]
    fn every_valid_fixture_projects_without_mutating_source()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aion-awl/tests/fixtures");
        let mut paths = Vec::new();
        collect_valid_fixtures(&fixtures, &mut paths)?;
        assert!(!paths.is_empty());
        for path in paths {
            let source = std::fs::read_to_string(&path)?;
            let original = source.clone();
            let document = parse(&source)
                .map_err(|error| format!("{} did not parse: {}", path.display(), error.message))?;
            let canonical = print(&document);
            assert_eq!(print(&parse(&canonical)?), canonical, "{}", path.display());
            let response = check_source_at(
                &CheckRequest {
                    source: source.clone(),
                    path: Some(path.to_string_lossy().into_owned()),
                },
                path.parent(),
            );
            assert_eq!(source, original, "projection mutated {}", path.display());
            assert!(
                response.ok,
                "{}: {:?}",
                path.display(),
                response.diagnostics
            );
            let graph = response
                .semantic
                .ok_or("valid fixture had no semantics")?
                .graph;
            assert_eq!(
                graph.steps.len(),
                document.steps.len(),
                "{}",
                path.display()
            );
        }
        Ok(())
    }

    fn collect_valid_fixtures(
        directory: &Path,
        found: &mut Vec<std::path::PathBuf>,
    ) -> std::io::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                collect_valid_fixtures(&path, found)?;
            } else if path.extension().is_some_and(|extension| extension == "awl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "valid")
            {
                found.push(path);
            }
        }
        Ok(())
    }
}
