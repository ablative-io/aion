//! Flow-vocabulary lowering coverage (the retired B2 refusal ratchet):
//! every rev-3 construct fed to the public emitter now lowers — B4
//! dismantled the gate construct by construct. Each probe document is the
//! minimal use of one construct; emission must succeed and carry the
//! construct's generated shape.

use std::error::Error;

use aion_awl::{check, emit, parse};

type TestResult = Result<(), Box<dyn Error>>;

fn line_col_of(source: &str, needle: &str) -> Result<(usize, usize), Box<dyn Error>> {
    let start = source
        .find(needle)
        .ok_or_else(|| format!("needle {needle:?} not found"))?;
    let prefix = &source[..start];
    let line = prefix.matches('\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    Ok((line, start - line_start + 1))
}

// ---------------------------------------------------------------------
// Emitter and MIR refusals: honest, spanned, "not yet lowered"
// ---------------------------------------------------------------------

/// Every new construct: (name, a check-clean document using it, the line
/// its refusal must anchor on).
fn refusal_documents() -> Vec<(&'static str, String, &'static str)> {
    let region = "//! Region.\n\
workflow t\n\
\x20 input items: [String]\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\n\
step wave\n\
\x20 distribute item in items\n\
\n\
step build\n\
\x20 work(item: item) -> note\n\
\n\
step gather\n\
\x20 collect note -> notes\n\
\x20 \"done\" |> route done\n";
    let sequence = region.replace("distribute item in items", "sequence item in items");
    let subflow = "//! Subflow.\n\
workflow t\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\n\
subflow s(item: String)\n\
\x20 outcome out: type String\n\
\x20 step run\n\
\x20   item |> route out\n\
\n\
step call_it\n\
\x20 s(item: task) -> got\n\
\n\
step finish_up\n\
\x20 got |> route done\n";
    let visits = "//! Visits.\n\
workflow t\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\n\
step first\n\
\x20 work(item: task) -> note\n\
\n\
step again\n\
\x20 work(item: note) -> extra\n\
\n\
\x20 outcome more: when extra == \"\", route first\n\
\x20 outcome stop: otherwise, route done(extra)\n\
\x20 max 2 visits\n";
    let value_payload = "//! Value payload.\n\
workflow t\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action work(item: String) -> String\n\
\n\
step run\n\
\x20 work(item: task) -> note\n\
\n\
\x20 outcome ok: otherwise, route done(note)\n";
    vec![
        ("distribute", region.to_owned(), "distribute item in items"),
        ("sequence", sequence, "sequence item in items"),
        ("subflow", subflow.to_owned(), "s(item: String)"),
        ("max visits", visits.to_owned(), "max 2 visits"),
        ("value payload", value_payload.to_owned(), "note)\n"),
    ]
}

/// The generated fragment each construct's emission must carry.
fn emitted_fragment(name: &str) -> &'static str {
    match name {
        "distribute" => "workflow.map(",
        "sequence" => "list.try_fold(",
        "subflow" => "awl_sf_s(",
        "max visits" => "awl_error.AwlVisitsExceeded(",
        "value payload" => "Ok(DoneOutcome(note))",
        _ => "execute",
    }
}

#[test]
fn every_new_construct_emits_with_its_generated_shape() -> TestResult {
    for (name, source, needle) in refusal_documents() {
        let document = parse(&source)?;
        let errors = check(&document);
        assert!(
            errors.is_empty(),
            "{name}: the probe document must check clean: {errors:#?}"
        );
        let _ = needle;
        let generated = emit(&document)
            .map_err(|error| format!("{name}: emit refused: {}", error.message))?;
        let fragment = emitted_fragment(name);
        assert!(
            generated.contains(fragment),
            "{name}: generated module misses `{fragment}`:\n{generated}"
        );
    }
    Ok(())
}

#[test]
fn every_new_construct_lowers_on_the_direct_path() -> TestResult {
    for (name, source, needle) in refusal_documents() {
        let document = parse(&source)?;
        let _ = needle;
        let module = aion_awl::mir::lower(&document, None)
            .map_err(|error| format!("{name}: lower refused: {error}"))?;
        aion_awl::mir::verify(&module).map_err(|error| format!("{name}: verify: {error}"))?;
    }
    Ok(())
}
