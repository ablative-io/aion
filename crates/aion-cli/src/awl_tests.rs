use aion_awl::CheckError;

use super::*;

/// A canonical rev-2 document: one input, one success outcome, one
/// worker action, one step whose pipe routes the result out.
const VALID_DOC: &str = "//! Probe: make a note of the token and hand it back.\n\
workflow probe\n\
\x20 input token: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker probe\n\
\x20 action make(token: String) -> String\n\
\n\
step one\n\
\x20 token |> make |> route done\n";

/// A canonical document using the B1 flow vocabulary: a multi-line raw
/// string, a `json { … }` literal, `schema of`, const folding, and an
/// expression-headed statement.
const VOCAB_DOC: &str = "//! Vocabulary probe.\n\
workflow vocab_probe\n\
\x20 input task: String\n\
\x20 outcome done: type String, route success\n\
\n\
const greeting = \"\"\"\n\
\x20 Hello from the vocabulary.\n\
\x20 \"\"\"\n\
const item_schema = json { \"type\": \"object\" }\n\
const verdict_schema = schema of Verdict\n\
const prompt = greeting + \" Task: \"\n\
\n\
type Verdict { passed: Bool }\n\
\n\
worker probe\n\
\x20 action make(prompt: String, output_schema: String) -> String\n\
\n\
step one\n\
\x20 make(prompt: prompt + task, output_schema: verdict_schema) -> made\n\
\x20 make(prompt: greeting, output_schema: item_schema) -> extra\n\
\x20 \"made: \" + made + extra -> summary\n\
\x20 summary |> route done\n";

/// Well-formed rev-2 source whose route names no declared outcome or
/// step — a typecheck error, not a parse error.
const BROKEN_ROUTE_DOC: &str = "//! Probe with a dangling route.\n\
workflow probe\n\
\x20 input token: String\n\
\x20 outcome done: type String, route success\n\
\n\
worker probe\n\
\x20 action make(token: String) -> String\n\
\n\
step one\n\
\x20 token |> make |> route missing\n";

#[test]
fn diagnostic_renders_the_compiler_style_line() {
    // A synthetic checker diagnostic renders as <file>:<line>:<column>.
    let error = CheckError {
        span: Span {
            start: 12,
            end: 16,
            line: 3,
            column: 7,
        },
        message: "unknown name `stat`".to_owned(),
    };
    let line = diagnostic(Path::new("flows/probe.awl"), error.span, &error.message);
    assert_eq!(line, "flows/probe.awl:3:7: error: unknown name `stat`");
}

#[test]
fn check_source_counts_steps_on_a_clean_document() {
    let steps = check_source(Path::new("probe.awl"), VALID_DOC);
    assert_eq!(steps, Ok(1));
}

#[test]
fn check_source_renders_a_parse_error_as_a_diagnostic() -> anyhow::Result<()> {
    let Err(diagnostics) = check_source(Path::new("probe.awl"), "not a workflow\n") else {
        anyhow::bail!("expected a parse diagnostic");
    };
    assert_eq!(diagnostics.len(), 1);
    assert!(
        diagnostics[0].starts_with("probe.awl:1:1: error: "),
        "unexpected diagnostic: {}",
        diagnostics[0]
    );
    Ok(())
}

#[test]
fn check_source_renders_typecheck_errors_as_diagnostics() -> anyhow::Result<()> {
    let Err(diagnostics) = check_source(Path::new("probe.awl"), BROKEN_ROUTE_DOC) else {
        anyhow::bail!("expected a typecheck diagnostic");
    };
    assert!(!diagnostics.is_empty());
    for line in &diagnostics {
        assert!(
            line.starts_with("probe.awl:") && line.contains(": error: "),
            "unexpected diagnostic: {line}"
        );
    }
    Ok(())
}

#[test]
fn format_source_is_the_canonical_printer() -> anyhow::Result<()> {
    // An already-canonical document formats to itself (one rendering).
    let formatted = format_source(Path::new("probe.awl"), VALID_DOC)
        .map_err(|d| anyhow::anyhow!("unexpected diagnostics: {d:?}"))?;
    assert_eq!(formatted, VALID_DOC);
    Ok(())
}

/// `aion awl check` accepts the B1 flow vocabulary (raw strings,
/// `json { … }`, `schema of`, consts, expression-headed statements).
#[test]
fn check_source_accepts_the_flow_vocabulary() {
    let steps = check_source(Path::new("vocab.awl"), VOCAB_DOC);
    assert_eq!(steps, Ok(1));
}

/// `aion awl fmt` is idempotent on the B1 flow vocabulary: the document
/// is already canonical, and formatting the formatted output changes
/// nothing.
#[test]
fn format_source_is_idempotent_on_the_flow_vocabulary() -> anyhow::Result<()> {
    let once = format_source(Path::new("vocab.awl"), VOCAB_DOC)
        .map_err(|d| anyhow::anyhow!("unexpected diagnostics: {d:?}"))?;
    assert_eq!(once, VOCAB_DOC);
    let twice = format_source(Path::new("vocab.awl"), &once)
        .map_err(|d| anyhow::anyhow!("unexpected diagnostics: {d:?}"))?;
    assert_eq!(twice, once);
    Ok(())
}

/// `aion awl emit` folds the vocabulary before lowering: the emitted
/// Gleam carries the folded strings, never a const name.
#[test]
fn emit_source_folds_the_flow_vocabulary() -> anyhow::Result<()> {
    let generated = emit_source(Path::new("vocab.awl"), VOCAB_DOC)
        .map_err(|d| anyhow::anyhow!("unexpected diagnostics: {d:?}"))?;
    assert!(
        generated.contains("Hello from the vocabulary."),
        "raw string content missing: {generated}"
    );
    assert!(
        !generated.contains("verdict_schema"),
        "unfolded const reference leaked: {generated}"
    );
    Ok(())
}

#[test]
fn format_source_reports_a_parse_error_without_output() -> anyhow::Result<()> {
    let Err(diagnostics) = format_source(Path::new("probe.awl"), "step\n") else {
        anyhow::bail!("expected a parse diagnostic");
    };
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].starts_with("probe.awl:1:"));
    Ok(())
}

#[test]
fn emit_source_generates_gleam_for_a_clean_document() -> anyhow::Result<()> {
    let generated = emit_source(Path::new("probe.awl"), VALID_DOC)
        .map_err(|d| anyhow::anyhow!("unexpected diagnostics: {d:?}"))?;
    assert!(
        generated.contains("pub fn execute"),
        "expected generated code to contain `pub fn execute`: {generated}"
    );
    assert!(
        generated.contains("make_activity(token)"),
        "expected the action dispatch in the generated module: {generated}"
    );
    Ok(())
}

#[test]
fn emit_output_writes_packaging_sidecar_for_implicit_children() -> anyhow::Result<()> {
    let source = "//! CLI structured artifact proof.\n\
workflow cli_parallel\n\
\x20 input items: [String]\n\
\x20 outcome done: type Done, route success\n\
\n\
type Done { count: Int }\n\
\n\
worker proof\n\
\x20 action first(item: String) -> String\n\
\x20 action second(item: String) -> String\n\
\n\
step fan\n\
\x20 distribute item in items\n\
step one\n\
\x20 first(item: item) -> prepared\n\
step two\n\
\x20 second(item: prepared) -> result\n\
step gather\n\
\x20 collect result -> results\n\
\x20 results |> count -> total\n\
\x20 route done(count: total)\n";
    let temp = tempfile::tempdir()?;
    let source_path = temp.path().join("cli_parallel.awl");
    let output_path = temp.path().join("cli_parallel.gleam");
    fs::write(&source_path, source)?;
    assert_eq!(
        emit_command(&source_path, Some(&output_path), EmitTarget::Gleam),
        ExitCode::SUCCESS,
        "the real CLI emit path failed"
    );
    assert!(fs::read_to_string(&output_path)?.contains("workflow.spawn"));

    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(output_path.with_extension("awl.json"))?)?;
    assert_eq!(metadata["entry_module"], "cli_parallel");
    assert_eq!(
        metadata["synthesized_workflows"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(
        metadata["synthesized_workflows"][0]["workflow_type"]
            .as_str()
            .is_some_and(
                |workflow_type| workflow_type.starts_with("aion_internal_awl_child_cli_parallel_")
            )
    );
    assert_eq!(
        metadata["synthesized_workflows"][0]["input_schema"]["type"],
        "object"
    );
    Ok(())
}

#[test]
fn emit_source_is_gated_on_a_clean_typecheck() -> anyhow::Result<()> {
    // Emission must refuse rather than generate code from an ill-typed
    // document.
    let Err(diagnostics) = emit_source(Path::new("probe.awl"), BROKEN_ROUTE_DOC) else {
        anyhow::bail!("expected a typecheck diagnostic");
    };
    assert!(!diagnostics.is_empty());
    for line in &diagnostics {
        assert!(
            line.starts_with("probe.awl:") && line.contains(": error: "),
            "unexpected diagnostic: {line}"
        );
    }
    Ok(())
}

#[test]
fn emit_source_renders_a_parse_error_as_a_diagnostic() -> anyhow::Result<()> {
    let Err(diagnostics) = emit_source(Path::new("probe.awl"), "not a workflow\n") else {
        anyhow::bail!("expected a parse diagnostic");
    };
    assert_eq!(diagnostics.len(), 1);
    assert!(
        diagnostics[0].starts_with("probe.awl:1:1: error: "),
        "unexpected diagnostic: {}",
        diagnostics[0]
    );
    Ok(())
}

/// A declared shorthand type with a `?` field derives its JSON Schema:
/// doc lines flow to `description`, `?` maps to "not in required".
#[test]
fn schema_source_derives_a_declared_type() -> anyhow::Result<()> {
    let source = "//! File a note.\n\
workflow filed_note\n\
\x20 input note: Note\n\
\x20 outcome kept: type Note, route success\n\
\n\
/// A note somebody jotted down.\n\
type Note {\n\
\x20 title: String,\n\
\x20 body: String?,\n\
}\n\
\n\
worker files\n\
\x20 action keep(note: Note) -> Note\n\
\n\
step keep_note\n\
\x20 note |> keep |> route kept\n";
    let schema = schema_source(Path::new("note.awl"), source, Some("Note"))
        .map_err(|diagnostics| anyhow::anyhow!("unexpected diagnostics: {diagnostics:?}"))?;
    let value: serde_json::Value = serde_json::from_str(&schema)?;
    assert_eq!(value["type"], "object");
    assert_eq!(value["description"], "A note somebody jotted down.");
    assert_eq!(value["required"], serde_json::json!(["title"]));
    assert_eq!(value["properties"]["body"]["type"], "string");
    Ok(())
}

/// Without `--type`, the workflow's start contract derives: one object
/// over the inputs, `?` inputs omitted from `required`.
#[test]
fn schema_source_without_type_emits_the_start_contract() -> anyhow::Result<()> {
    let source = "//! Greet, optionally loudly.\n\
workflow greeter\n\
\x20 input name: String\n\
\x20 input flair: String?\n\
\x20 outcome done: type String, route success\n\
\n\
worker greeter\n\
\x20 action greet(name: String) -> String\n\
\n\
step greet\n\
\x20 name |> greet |> route done\n";
    let schema = schema_source(Path::new("greeter.awl"), source, None)
        .map_err(|diagnostics| anyhow::anyhow!("unexpected diagnostics: {diagnostics:?}"))?;
    let value: serde_json::Value = serde_json::from_str(&schema)?;
    assert_eq!(value["properties"]["name"]["type"], "string");
    assert_eq!(value["required"], serde_json::json!(["name"]));
    Ok(())
}

#[test]
fn schema_source_is_gated_on_a_clean_typecheck() -> anyhow::Result<()> {
    let source = "//! Probe with an undeclared field type.\n\
workflow probe\n\
\x20 input token: String\n\
\x20 outcome done: type Brief, route success\n\
\n\
type Brief { value: Missing }\n\
\n\
worker probe\n\
\x20 action make(token: String) -> Brief\n\
\n\
step one\n\
\x20 token |> make |> route done\n";
    let Err(diagnostics) = schema_source(Path::new("probe.awl"), source, Some("Brief")) else {
        anyhow::bail!("expected a typecheck diagnostic");
    };
    assert!(!diagnostics.is_empty());
    for line in &diagnostics {
        assert!(
            line.starts_with("probe.awl:") && line.contains(": error: "),
            "unexpected diagnostic: {line}"
        );
    }
    Ok(())
}

/// The gleam target is byte-identical to the pre-`--target` behaviour: the real
/// `emit_command` writing `--target gleam` produces exactly the bytes the
/// legacy `emit_source` seam does — the target split changed no gleam output.
#[test]
fn emit_gleam_target_is_byte_identical_to_the_legacy_seam() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let source_path = temp.path().join("probe.awl");
    let output_path = temp.path().join("probe.gleam");
    fs::write(&source_path, VALID_DOC)?;
    assert_eq!(
        emit_command(&source_path, Some(&output_path), EmitTarget::Gleam),
        ExitCode::SUCCESS,
        "the gleam emit path failed"
    );
    let written = fs::read_to_string(&output_path)?;
    let legacy = emit_source(&source_path, VALID_DOC)
        .map_err(|d| anyhow::anyhow!("legacy emit failed: {d:?}"))?;
    assert_eq!(written, legacy, "gleam target output drifted from the seam");
    Ok(())
}

/// `--target beam` refuses without `--output`: BEAM bytes are never written to
/// stdout, so a missing output is a typed failure, not a stdout dump.
#[test]
fn emit_beam_refuses_without_output() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let source_path = temp.path().join("probe.awl");
    fs::write(&source_path, VALID_DOC)?;
    assert_eq!(
        emit_command(&source_path, None, EmitTarget::Beam),
        ExitCode::FAILURE,
        "beam emit to stdout must be refused"
    );
    Ok(())
}

/// `--target beam` writes one BEAM container and a beam-shaped sidecar: the
/// module bytes lead with the `FOR1` magic, and the sidecar carries the derived
/// contracts and action requirements — never the Gleam `project_metadata`
/// shape (no `format_version`/`entry_module` keys next to `.beam` bytes).
#[test]
fn emit_beam_writes_a_module_and_a_beam_shaped_sidecar() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let source_path = temp.path().join("probe.awl");
    let output_path = temp.path().join("probe.beam");
    fs::write(&source_path, VALID_DOC)?;
    assert_eq!(
        emit_command(&source_path, Some(&output_path), EmitTarget::Beam),
        ExitCode::SUCCESS,
        "the beam emit path failed"
    );

    let module = fs::read(&output_path)?;
    assert!(
        module.starts_with(b"FOR1"),
        "the beam output is not a BEAM container"
    );

    let sidecar_path = output_path.with_file_name("probe.beam.json");
    let sidecar: serde_json::Value = serde_json::from_slice(&fs::read(&sidecar_path)?)?;
    assert_eq!(sidecar["target"], "beam");
    assert_eq!(sidecar["workflow_name"], "probe");
    assert_eq!(sidecar["input_schema"]["type"], "object");
    assert!(sidecar["output_schema"].is_object());
    assert!(sidecar["actions"].is_array(), "action requirements missing");
    assert!(
        sidecar.get("format_version").is_none() && sidecar.get("entry_module").is_none(),
        "beam sidecar leaked the Gleam project_metadata shape: {sidecar}"
    );
    Ok(())
}

/// The ops-console compatibility proof (the operator's condition): the bytes the
/// CLI writes for `--target beam` are byte-identical to the entry module bytes
/// inside `compile_and_assemble_awl`'s archive for the same source. One seam,
/// zero drift — CLI output and console-deployed output can never diverge.
#[test]
fn emit_beam_bytes_equal_the_archive_entry_module() -> anyhow::Result<()> {
    use aion_package::{ExtractionLimits, Package};

    let temp = tempfile::tempdir()?;
    let source_path = temp.path().join("probe.awl");
    let output_path = temp.path().join("probe.beam");
    fs::write(&source_path, VALID_DOC)?;
    assert_eq!(
        emit_command(&source_path, Some(&output_path), EmitTarget::Beam),
        ExitCode::SUCCESS,
        "the beam emit path failed"
    );
    let cli_bytes = fs::read(&output_path)?;

    let root = source_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("source path has no parent"))?;
    let prepared = aion_awl_package::compile_and_assemble_awl(VALID_DOC, root)?;
    let package = Package::load_from_bytes(prepared.archive, ExtractionLimits::unbounded())?;
    let entry_module = package.manifest().entry_module.clone();
    let archive_bytes = package
        .beams()
        .get(&entry_module)
        .ok_or_else(|| anyhow::anyhow!("archive lost its entry module {entry_module}"))?;

    assert_eq!(
        cli_bytes.as_slice(),
        archive_bytes,
        "CLI beam bytes drifted from the archive entry module — the seam split"
    );
    Ok(())
}
