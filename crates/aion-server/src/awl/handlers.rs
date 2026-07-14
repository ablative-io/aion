use std::path::Path;

use aion_awl::{Span, parse, print, semantic};
use serde::{Deserialize, Serialize};

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
    let root = request
        .path
        .as_deref()
        .and_then(|path| Path::new(path).parent())
        .filter(|path| !path.as_os_str().is_empty());
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
    let graph = super::projection::build(&document);
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
            let response = check_source(&CheckRequest {
                source: source.clone(),
                path: Some(path.to_string_lossy().into_owned()),
            });
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
