//! Pipe-lowering pins: combinator stages lower to the `gleam/list` runtime
//! surface with lifted projection/compare closures, post-stage `any`/`all`
//! ride the shared predicate machinery, and a child-naming pipe stage lowers
//! to the string-name `spawn_and_wait` shape with a planned witness.

use std::fs;
use std::path::{Path, PathBuf};

use super::{LowerError, MirFn, MirModule, lower, print_mir, verify};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lower_source(source: &str) -> Result<Result<MirModule, LowerError>, Box<dyn std::error::Error>> {
    let document = crate::parse(source)?;
    Ok(lower(&document, Some(Path::new("."))))
}

#[test]
fn combinator_stages_lower_to_list_runtime_calls() -> Result<(), Box<dyn std::error::Error>> {
    let path = manifest_dir().join("tests/fixtures/rev2/step-bodies/valid/combinators.awl");
    let source = fs::read_to_string(&path)?;
    let document = crate::parse(&source)?;
    let module = lower(&document, path.parent())?;
    verify(&module)?;
    let text = print_mir(&module);
    for token in [
        "gleam@list:filter/2",
        "gleam@list:sort/2",
        "gleam@list:map/2",
        "gleam@list:length/1",
        "gleam@int:compare/2",
        "make_closure",
        "awl_combinator_",
    ] {
        assert!(text.contains(token), "missing `{token}` in:\n{text}");
    }
    Ok(())
}

#[test]
fn sort_string_key_selects_string_compare() -> Result<(), Box<dyn std::error::Error>> {
    let source = "\
//! Sort by a string key.
workflow sort_by_title
  input docs: [Doc]
  outcome listed: type Listed, route success

type Doc    { title: String }
type Listed { titles: [String] }

step order
  docs |> sort(.title) |> map(.title) -> titles
  route listed(titles: titles)
";
    let module = lower_source(source)??;
    verify(&module)?;
    let text = print_mir(&module);
    assert!(
        text.contains("gleam@string:compare/2"),
        "a String sort key must select string.compare:\n{text}"
    );
    Ok(())
}

#[test]
fn post_stage_any_all_lower_through_predicate_closures() -> Result<(), Box<dyn std::error::Error>> {
    let source = "\
//! Post-stage any/all through the shared predicate machinery.
workflow post_stage_quantifiers
  input findings: [Finding]
  outcome judged: type Judged, route success

type Finding { flag: Bool, n: Int }
type Judged  { any_high: Bool, all_high: Bool }

step judge
  findings |> filter(.flag) |> any(.n >= 3) -> any_high
  findings |> filter(.flag) |> all(.n >= 3) -> all_high
  route judged(any_high: any_high, all_high: all_high)
";
    let module = lower_source(source)??;
    verify(&module)?;
    let text = print_mir(&module);
    for token in ["gleam@list:any/2", "gleam@list:all/2", "awl_predicate_"] {
        assert!(text.contains(token), "missing `{token}` in:\n{text}");
    }
    Ok(())
}

#[test]
fn pipe_child_stage_lowers_to_spawn_and_wait() -> Result<(), Box<dyn std::error::Error>> {
    let source = "\
//! Pipe-into-child MIR pin.
workflow pipe_child
  input essay: String
  outcome noted: type Note, route success

type Grade { score: Int, feedback: String }
type Note  { feedback: String }

child score_essay(essay: String) -> Grade

step delegate
  essay |> score_essay |> .feedback -> feedback
  route noted(feedback: feedback)
";
    let module = lower_source(source)??;
    verify(&module)?;
    let text = print_mir(&module);
    for token in [
        "aion@workflow:spawn_and_wait/6",
        "aion@awl@error:map_child_error/1",
    ] {
        assert!(text.contains(token), "missing `{token}` in:\n{text}");
    }
    // The pipe stage alone must plan the module-local child witness.
    let has_witness = module.functions.iter().any(|function| match function {
        MirFn::Flow(flow) => flow.name == "awl$child_witness",
        MirFn::Templated { name, .. } => name == "awl$child_witness",
    });
    assert!(
        has_witness,
        "a child-naming pipe stage must plan the child witness:\n{text}"
    );
    Ok(())
}
