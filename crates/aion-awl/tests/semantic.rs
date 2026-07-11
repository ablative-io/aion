//! Direct contracts for checker-owned semantic span queries.

use std::error::Error;

use aion_awl::semantic::{DeclarationKind, SemanticAnalysis, analyze};

type TestResult = Result<(), Box<dyn Error>>;

const SOURCE: &str = "\
//! Semantic query coverage.
workflow semantic_probe
  input topics: [String]
  input max_rounds: Int
  outcome done: type Out, route success

type Out { text: String }
type Draft { body: String, ready: Bool }

worker local
  action make(topic: String) -> Draft
  action finish(draft: Draft) -> Out

step fan
  fork topic in topics
    make(topic: topic) -> made
  join -> drafts

step iterate
  loop draft = Draft(body: drafts[0].body, ready: false) counting rounds
    make(topic: draft.body) -> draft
    until draft.ready
    max max_rounds
  outcome ready: when draft.ready, route finalize_step
  outcome exhausted: otherwise, route finalize_step

step finalize_step
  finish(draft: draft) -> out
  out |> route done
";

fn offset(source: &str, needle: &str) -> Result<usize, Box<dyn Error>> {
    source
        .find(needle)
        .map(|start| start + needle.len() - 1)
        .ok_or_else(|| format!("missing source needle {needle:?}").into())
}

fn assert_info(
    analysis: &SemanticAnalysis,
    source: &str,
    needle: &str,
    ty: &str,
    kind: DeclarationKind,
    name: &str,
) -> TestResult {
    let info = analysis
        .info_at(offset(source, needle)?)
        .ok_or_else(|| format!("no semantic info at {needle:?}"))?;
    assert_eq!(info.ty.as_deref(), Some(ty), "type at {needle:?}");
    let declaration = info
        .declaration
        .as_ref()
        .ok_or_else(|| format!("no declaration at {needle:?}"))?;
    assert_eq!(declaration.kind, kind, "declaration kind at {needle:?}");
    assert_eq!(declaration.name, name, "declaration name at {needle:?}");
    Ok(())
}

#[test]
fn semantic_spans_cover_declarations_and_checker_owned_scopes() -> TestResult {
    let document = aion_awl::parse(SOURCE).map_err(|error| error.message)?;
    let analysis = analyze(&document);
    assert!(
        analysis.diagnostics().is_empty(),
        "semantic fixture must check cleanly: {:#?}",
        analysis.diagnostics()
    );

    let cases = [
        ("input topics", "[String]", DeclarationKind::Input, "topics"),
        (
            "type Draft { body",
            "String",
            DeclarationKind::Field,
            "body",
        ),
        (
            "action make(topic",
            "String",
            DeclarationKind::Parameter,
            "topic",
        ),
        (") -> Draft", "Draft", DeclarationKind::Type, "Draft"),
        ("-> made", "Draft", DeclarationKind::Binding, "made"),
        ("topic: topic", "String", DeclarationKind::Binding, "topic"),
        ("in topics", "[String]", DeclarationKind::Input, "topics"),
        (
            "join -> drafts",
            "[Draft]",
            DeclarationKind::Binding,
            "drafts",
        ),
        (
            "body: drafts",
            "[Draft]",
            DeclarationKind::Binding,
            "drafts",
        ),
        ("loop draft", "Draft", DeclarationKind::Binding, "draft"),
        ("draft: draft", "Draft", DeclarationKind::Binding, "draft"),
        ("draft.body", "String", DeclarationKind::Field, "body"),
        ("route done", "Out", DeclarationKind::Outcome, "done"),
    ];
    for (needle, ty, kind, name) in cases {
        assert_info(&analysis, SOURCE, needle, ty, kind, name)?;
    }
    Ok(())
}
