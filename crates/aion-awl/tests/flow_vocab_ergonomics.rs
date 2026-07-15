//! Flow-vocabulary B1 ergonomics: raw strings, `json { … }` literals,
//! document-level `const`, `schema of`, and the expression-headed statement
//! fix — span-exact diagnostics, lossless printing, fold-through-emission,
//! and the semantic index's `const` declaration kind.

use std::error::Error;

use aion_awl::semantic::{DeclarationKind, analyze};
use aion_awl::{check, emit, parse, print};

type TestResult = Result<(), Box<dyn Error>>;

/// A canonical document exercising all four B1 features at once.
const VOCAB_DOC: &str = "//! Vocabulary proof.\n\
workflow vocab_proof\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
const greeting = \"\"\"\n\
\x20 Hello from the vocabulary.\n\
\x20 \"\"\"\n\
const item_schema = json {\n\
\x20 \"type\": \"object\",\n\
\x20 \"properties\": { \"id\": { \"type\": \"string\" } }\n\
}\n\
const verdict_schema = schema of Verdict\n\
const prompt = greeting + \" Task: \"\n\
\n\
type Verdict { passed: Bool }\n\
\n\
worker vocab\n\
\x20 action run_agent(prompt: String, output_schema: String) -> String\n\
\n\
step run\n\
\x20 run_agent(prompt: prompt + task, output_schema: verdict_schema) -> result\n\
\x20 run_agent(prompt: greeting, output_schema: item_schema) -> extra\n\
\x20 \"tagged: \" + result + extra -> tagged\n\
\x20 tagged |> route done\n";

fn first_error(source: &str) -> Result<(usize, usize, String), Box<dyn Error>> {
    let document = parse(source)?;
    let errors = check(&document);
    let error = errors.first().ok_or("expected a check error")?;
    Ok((error.span.line, error.span.column, error.message.clone()))
}

// ---------------------------------------------------------------------
// Lexer: raw strings and json bodies
// ---------------------------------------------------------------------

#[test]
fn unterminated_raw_string_is_refused_at_the_opener() -> TestResult {
    let source = "//! Broken.\n\
workflow broken\n\
\x20 outcome done: type String, route success\n\
\n\
const greeting = \"\"\"\n\
\x20 never closed\n";
    let Err(error) = parse(source) else {
        return Err("expected a parse error".into());
    };
    assert_eq!(error.message, "unterminated raw string literal");
    assert_eq!((error.span.line, error.span.column), (5, 18));
    Ok(())
}

#[test]
fn unterminated_json_body_is_refused_at_the_open_brace() -> TestResult {
    let source = "//! Broken.\n\
workflow broken\n\
\x20 outcome done: type String, route success\n\
\n\
const shape = json {\n\
\x20 \"open\": true\n";
    let Err(error) = parse(source) else {
        return Err("expected a parse error".into());
    };
    assert_eq!(error.message, "unterminated `json { … }` literal body");
    assert_eq!((error.span.line, error.span.column), (5, 20));
    Ok(())
}

#[test]
fn raw_string_value_is_verbatim_with_no_escape_processing() -> TestResult {
    let source = "//! Raw.\n\
workflow raw\n\
\x20 outcome done: type String, route success\n\
\n\
const text = \"\"\"a \\n stays two chars, \"quotes\" stay\"\"\"\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 text |> stamp |> route done\n";
    let document = parse(source)?;
    let decl = document.consts.first().ok_or("expected a const")?;
    let aion_awl::Expr::RawString { value, .. } = &decl.value else {
        return Err("expected a raw string value".into());
    };
    assert_eq!(value, "a \\n stays two chars, \"quotes\" stay");
    Ok(())
}

#[test]
fn json_body_brace_counting_respects_braces_inside_strings() -> TestResult {
    let source = "//! Braces.\n\
workflow braces\n\
\x20 outcome done: type String, route success\n\
\n\
const shape = json { \"text\": \"{ not { a } brace }\", \"n\": 1 }\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 shape |> stamp |> route done\n";
    let document = parse(source)?;
    let decl = document.consts.first().ok_or("expected a const")?;
    let aion_awl::Expr::Json { body, .. } = &decl.value else {
        return Err("expected a json value".into());
    };
    assert_eq!(body, "{ \"text\": \"{ not { a } brace }\", \"n\": 1 }");
    assert!(check(&document).is_empty(), "document must check clean");
    Ok(())
}

// ---------------------------------------------------------------------
// Parser: a statement may start with any expression
// ---------------------------------------------------------------------

#[test]
fn a_statement_may_start_with_a_string_literal() -> TestResult {
    let source = "//! Literal head.\n\
workflow literal_head\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 \"prefix: \" + task -> labelled\n\
\x20 labelled |> stamp |> route done\n";
    let document = parse(source)?;
    assert!(check(&document).is_empty(), "document must check clean");
    // And the canonical printer round-trips it byte-for-byte.
    assert_eq!(print(&document), source);
    Ok(())
}

#[test]
fn an_expression_headed_statement_still_needs_a_terminator() -> TestResult {
    let source = "//! Literal head.\n\
workflow literal_head\n\
\x20 outcome done: type String, route success\n\
\n\
step run\n\
\x20 \"dangling\"\n";
    let Err(error) = parse(source) else {
        return Err("expected a parse error".into());
    };
    assert!(
        error.message.contains("unterminated pipe chain"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!((error.span.line, error.span.column), (6, 3));
    Ok(())
}

// ---------------------------------------------------------------------
// Checker: span-exact diagnostics for every new error class
// ---------------------------------------------------------------------

#[test]
fn invalid_json_body_points_into_the_body() -> TestResult {
    let source = "//! Bad JSON.\n\
workflow bad_json\n\
\x20 outcome done: type String, route success\n\
\n\
const shape = json {\n\
\x20 \"properties\": nope\n\
}\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 shape |> stamp |> route done\n";
    let (line, column, message) = first_error(source)?;
    assert!(
        message.contains("not valid JSON"),
        "unexpected message: {message}"
    );
    // The span lands INSIDE the body (line 6 is the body's second line;
    // `serde_json` anchors on the character where the ident parse failed).
    assert_eq!((line, column), (6, 18));
    Ok(())
}

#[test]
fn const_cycles_are_rejected_at_the_backward_reference() -> TestResult {
    let source = "//! Cycle.\n\
workflow cycle\n\
\x20 outcome done: type String, route success\n\
\n\
const first = second + \"!\"\n\
const second = first + \"?\"\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 first |> stamp |> route done\n";
    let (line, column, message) = first_error(source)?;
    assert_eq!(
        message,
        "const `first` is defined in terms of itself — const values cannot cycle"
    );
    assert_eq!((line, column), (6, 16));
    Ok(())
}

#[test]
fn undefined_const_references_are_rejected() -> TestResult {
    let source = "//! Undefined.\n\
workflow undefined\n\
\x20 outcome done: type String, route success\n\
\n\
const prompt = missing + \"!\"\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 prompt |> stamp |> route done\n";
    let (line, column, message) = first_error(source)?;
    assert_eq!(
        message,
        "unknown const `missing` — a `const` value may reference only other consts"
    );
    assert_eq!((line, column), (5, 16));
    Ok(())
}

#[test]
fn duplicate_consts_are_rejected_at_the_second_declaration() -> TestResult {
    let source = "//! Duplicate.\n\
workflow duplicate\n\
\x20 outcome done: type String, route success\n\
\n\
const prompt = \"first\"\n\
const prompt = \"second\"\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 prompt |> stamp |> route done\n";
    let (line, column, message) = first_error(source)?;
    assert_eq!(message, "duplicate const declaration `prompt`");
    assert_eq!((line, column), (6, 7));
    Ok(())
}

#[test]
fn bindings_cannot_shadow_consts() -> TestResult {
    let source = "//! Shadow.\n\
workflow shadow\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
const prompt = \"fixed\"\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 stamp(text: task) -> prompt\n\
\x20 prompt |> stamp |> route done\n";
    let (line, column, message) = first_error(source)?;
    assert_eq!(
        message,
        "`prompt` is a document-level `const` — bindings cannot shadow consts"
    );
    assert_eq!((line, column), (12, 24));
    Ok(())
}

#[test]
fn schema_of_an_undeclared_type_is_rejected() -> TestResult {
    let source = "//! Unknown type.\n\
workflow unknown_type\n\
\x20 outcome done: type String, route success\n\
\n\
const shape = schema of Missing\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 shape |> stamp |> route done\n";
    let (line, column, message) = first_error(source)?;
    assert_eq!(message, "unknown type `Missing`");
    assert_eq!((line, column), (5, 25));
    Ok(())
}

#[test]
fn const_values_reject_runtime_reads() -> TestResult {
    let source = "//! Runtime.\n\
workflow runtime\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
const prompt = task + \"!\"\n\
\n\
worker w\n\
\x20 action stamp(text: String) -> String\n\
\n\
step run\n\
\x20 prompt |> stamp |> route done\n";
    let (line, column, message) = first_error(source)?;
    assert!(
        message.contains("is a workflow input or signal, not a const"),
        "unexpected message: {message}"
    );
    assert_eq!((line, column), (6, 16));
    Ok(())
}

// ---------------------------------------------------------------------
// Printer: lossless round-trip and idempotency over the new syntax
// ---------------------------------------------------------------------

#[test]
fn the_vocabulary_document_round_trips_byte_identically() -> TestResult {
    let tree = parse(VOCAB_DOC)?;
    assert!(check(&tree).is_empty(), "document must check clean");
    let printed = print(&tree);
    assert_eq!(printed, VOCAB_DOC);
    // parse → print → parse yields an identical tree, spans included.
    assert_eq!(parse(&printed)?, tree);
    // fmt is idempotent on the new features.
    assert_eq!(print(&parse(&printed)?), printed);
    Ok(())
}

// ---------------------------------------------------------------------
// Fold: the new expressions reach the emitter as folded strings
// ---------------------------------------------------------------------

#[test]
fn consts_fold_through_emission_as_plain_strings() -> TestResult {
    let generated = emit(&parse(VOCAB_DOC)?)?;
    // The raw string's content (with its literal newlines re-escaped by the
    // Gleam string printer) reaches the generated module.
    assert!(
        generated.contains("Hello from the vocabulary."),
        "raw string content missing from: {generated}"
    );
    // The concat of consts folds: greeting + " Task: " becomes one literal.
    assert!(
        generated.contains(" Task: "),
        "folded concat missing from: {generated}"
    );
    // `schema of Verdict` becomes the derived JSON Schema text.
    assert!(
        generated.contains(r#"\"required\":"#) && generated.contains("passed"),
        "derived schema missing from: {generated}"
    );
    // The json literal body rides through verbatim (escaped for Gleam).
    assert!(
        generated.contains(r#"\"properties\": { \"id\""#),
        "json body missing from: {generated}"
    );
    // No unresolved const references leak into the module.
    assert!(
        !generated.contains("verdict_schema") && !generated.contains("item_schema"),
        "unfolded const reference leaked into: {generated}"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// Semantic index: the const declaration kind
// ---------------------------------------------------------------------

#[test]
fn the_semantic_index_declares_consts_with_their_folded_type() -> TestResult {
    let document = parse(VOCAB_DOC)?;
    let analysis = analyze(&document);
    assert!(analysis.diagnostics().is_empty());
    let decl = document.consts.first().ok_or("expected a const")?;
    let info = analysis
        .info_for_span(decl.name_span)
        .ok_or("expected semantic facts on the const name")?;
    let declaration = info
        .declaration
        .as_ref()
        .ok_or("expected a declaration on the const name")?;
    assert_eq!(declaration.kind, DeclarationKind::Const);
    assert_eq!(declaration.name, "greeting");
    assert_eq!(info.ty.as_deref(), Some("String"));

    // A reference site resolves back to the const declaration.
    let step = document.steps.first().ok_or("expected a step")?;
    let aion_awl::Statement::Call(call) = step.body.first().ok_or("expected a call")? else {
        return Err("expected a call statement".into());
    };
    let arg = call
        .call
        .args
        .iter()
        .find(|arg| arg.name == "output_schema")
        .ok_or("expected the output_schema argument")?;
    let reference = analysis
        .info_for_span(aion_awl::Spanned::span(&arg.value))
        .and_then(|info| info.declaration.as_ref())
        .ok_or("expected the reference to resolve")?;
    assert_eq!(reference.kind, DeclarationKind::Const);
    assert_eq!(reference.name, "verdict_schema");
    Ok(())
}
