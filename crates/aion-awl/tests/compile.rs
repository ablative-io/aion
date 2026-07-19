//! B-1 proof obligations for the stable compile seam: `awl_hello` end to
//! end (bytes, contracts, actions, determinism), call-site node overrides in
//! the requirement derivation, and lossless diagnostic passthrough for both
//! lowering refusals and language errors.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use aion_awl::mir::{LowerError, lower, select};
use aion_awl::{CompileError, Span, action_requirements, compile};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn fixture(relative: &str) -> PathBuf {
    manifest_dir().join("tests/fixtures/rev2").join(relative)
}

fn unhex(text: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let text = text.trim();
    Ok((0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()?)
}

type BeamChunk = (String, Vec<u8>);

/// Splits a BEAM container into its `(name, payload)` chunk sequence.
fn beam_chunks(bytes: &[u8]) -> Result<Vec<BeamChunk>, Box<dyn std::error::Error>> {
    assert!(bytes.starts_with(b"FOR1"), "not a BEAM container");
    let mut chunks = Vec::new();
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let name = String::from_utf8(bytes[offset..offset + 4].to_vec())?;
        let size_bytes: [u8; 4] = bytes[offset + 4..offset + 8].try_into()?;
        let size = u32::from_be_bytes(size_bytes);
        let payload = bytes[offset + 8..offset + 8 + size as usize].to_vec();
        chunks.push((name, payload));
        offset += 8 + (size as usize).div_ceil(4) * 4;
    }
    Ok(chunks)
}

/// `LitT` table content behind either container form: a zero u32 size prefix
/// marks the raw uncompressed table (beamr 0.15.3's ENC-001 determinism form),
/// while a nonzero prefix declares the decompressed size of the zlib stream
/// that follows (the pre-0.15.3 form the committed goldens carry).
fn litt_content(payload: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let declared_bytes: [u8; 4] = payload[..4].try_into()?;
    let declared = u32::from_be_bytes(declared_bytes);
    if declared == 0 {
        return Ok(payload[4..].to_vec());
    }
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&payload[4..]), &mut out)?;
    if out.len() != declared as usize {
        return Err(format!(
            "LitT declared decompressed size {declared} but the stream inflated to {} bytes",
            out.len()
        )
        .into());
    }
    Ok(out)
}

/// Byte-equality per chunk, except `LitT`: its container form varies with the
/// beamr version (compressed pre-0.15.3, zero-prefix uncompressed after
/// ENC-001) and its deflate stream varies with the cargo feature graph. The
/// literal table content is the contract.
fn beam_equivalent(actual: &[u8], expected: &[u8]) -> TestResult {
    let actual_chunks = beam_chunks(actual)?;
    let expected_chunks = beam_chunks(expected)?;
    let names = |chunks: &[BeamChunk]| {
        chunks
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&actual_chunks),
        names(&expected_chunks),
        "chunk sequence drifted from the committed golden"
    );
    for ((name, actual_payload), (_, expected_payload)) in
        actual_chunks.iter().zip(expected_chunks.iter())
    {
        if name == "LitT" {
            assert_eq!(
                litt_content(actual_payload)?,
                litt_content(expected_payload)?,
                "LitT literal content drifted from the committed golden"
            );
        } else {
            assert_eq!(
                actual_payload, expected_payload,
                "chunk {name} drifted from the committed golden"
            );
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<(String, PathBuf), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)?;
    let root = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?
        .to_path_buf();
    Ok((source, root))
}

/// Obligation 1: `awl_hello` compiles; the bytes match the committed
/// `awl_hello.beam.hex` golden chunk-canonically (`assert_beam_equivalent`:
/// every chunk byte-equal except `LitT`, which compares decompressed; set
/// `AWL_BC2_BLESS=1` to re-bless — never implicit) AND the direct MIR path's
/// `select` output; the sidecar matches the committed `.gleam_types.hex`
/// golden; two calls are identical.
#[test]
fn awl_hello_compiles_deterministically_to_the_select_bytes() -> TestResult {
    let (source, root) = read(&fixture("flagship/valid/awl_hello.awl"))?;
    let first = compile(&source, &root).map_err(|error| error.to_string())?;
    let second = compile(&source, &root).map_err(|error| error.to_string())?;

    assert_eq!(first.workflow_name, "awl_hello");
    assert!(first.beam_bytes.starts_with(b"FOR1"));
    assert_eq!(first.beam_bytes, second.beam_bytes, "beam bytes drift");
    assert_eq!(first.sidecar_bytes, second.sidecar_bytes, "sidecar drift");
    assert_eq!(first, second, "compile is not a pure function of source");

    let document = aion_awl::parse(&source)?;
    let module = lower(&document, Some(&root))?;
    assert_eq!(
        first.beam_bytes,
        select(&module)?,
        "compile bytes differ from the direct MIR path"
    );
    assert!(!first.sidecar_bytes.is_empty(), "sidecar bytes are empty");

    let golden_root = manifest_dir().join("tests/mir-goldens/flagship/valid");
    let beam_golden = golden_root.join("awl_hello.beam.hex");
    let beam_hex = hex(&first.beam_bytes);
    if std::env::var("AWL_BC2_BLESS").is_ok() {
        fs::write(&beam_golden, &beam_hex)?;
    }
    let expected = fs::read_to_string(&beam_golden).map_err(|error| {
        format!(
            "missing beam golden {} (run with AWL_BC2_BLESS=1 to create): {error}",
            beam_golden.display()
        )
    })?;
    beam_equivalent(&first.beam_bytes, &unhex(&expected)?)?;
    assert_eq!(
        hex(&first.sidecar_bytes),
        fs::read_to_string(golden_root.join("awl_hello.gleam_types.hex"))?,
        "sidecar bytes differ from the committed golden"
    );
    Ok(())
}

/// BC-2b-4 flagship BEAM golden: the parallel-action fork fixture compiles
/// deterministically, the bytes equal the direct MIR path's `select` output,
/// and they match the committed `fork_action_fanout.beam.hex` golden
/// chunk-canonically (every chunk byte-equal except `LitT`, compared
/// decompressed; `AWL_BC2_BLESS=1` re-blesses — never implicit).
#[test]
fn fork_action_fanout_compiles_deterministically_to_the_select_bytes() -> TestResult {
    let (source, root) = read(&fixture("dag-fork/valid/fork_action_fanout.awl"))?;
    let first = compile(&source, &root).map_err(|error| error.to_string())?;
    let second = compile(&source, &root).map_err(|error| error.to_string())?;

    assert_eq!(first.workflow_name, "fork_action_fanout");
    assert!(first.beam_bytes.starts_with(b"FOR1"));
    assert_eq!(first.beam_bytes, second.beam_bytes, "beam bytes drift");
    assert_eq!(first, second, "compile is not a pure function of source");

    let document = aion_awl::parse(&source)?;
    let module = lower(&document, Some(&root))?;
    assert_eq!(
        first.beam_bytes,
        select(&module)?,
        "compile bytes differ from the direct MIR path"
    );

    let golden_root = manifest_dir().join("tests/mir-goldens/dag-fork/valid");
    let beam_golden = golden_root.join("fork_action_fanout.beam.hex");
    let beam_hex = hex(&first.beam_bytes);
    if std::env::var("AWL_BC2_BLESS").is_ok() {
        fs::write(&beam_golden, &beam_hex)?;
    }
    let expected = fs::read_to_string(&beam_golden).map_err(|error| {
        format!(
            "missing beam golden {} (run with AWL_BC2_BLESS=1 to create): {error}",
            beam_golden.display()
        )
    })?;
    beam_equivalent(&first.beam_bytes, &unhex(&expected)?)?;
    assert_eq!(
        hex(&first.sidecar_bytes),
        fs::read_to_string(golden_root.join("fork_action_fanout.gleam_types.hex"))?,
        "sidecar bytes differ from the committed golden"
    );
    Ok(())
}

/// Obligation 2: the derived contracts. The input schema is the existing
/// start-contract derivation; the output schema matches the hand-authored
/// reference envelope exactly (`Value` equality is key-order-insensitive).
#[test]
fn awl_hello_contracts_match_the_reference_shapes() -> TestResult {
    let (source, root) = read(&fixture("flagship/valid/awl_hello.awl"))?;
    let compiled = compile(&source, &root).map_err(|error| error.to_string())?;

    let document = aion_awl::parse(&source)?;
    assert_eq!(
        compiled.input_schema,
        aion_awl::schema_for_workflow_in(&document, &root)?,
        "input schema differs from the start-contract derivation"
    );

    let reference_path = manifest_dir().join("../../examples/awl-hello/schemas/output.json");
    let reference: serde_json::Value = serde_json::from_str(&fs::read_to_string(reference_path)?)?;
    assert_eq!(
        compiled.output_schema, reference,
        "derived outcome envelope differs from examples/awl-hello/schemas/output.json"
    );
    Ok(())
}

/// Obligation 3: the `awl_hello` action requirements, in declaration order.
#[test]
fn awl_hello_actions_are_the_two_unpinned_requirements() -> TestResult {
    let (source, root) = read(&fixture("flagship/valid/awl_hello.awl"))?;
    let compiled = compile(&source, &root).map_err(|error| error.to_string())?;
    let rows: Vec<(&str, &str, Option<&str>)> = compiled
        .actions
        .iter()
        .map(|req| {
            (
                req.task_queue.as_str(),
                req.action.as_str(),
                req.node.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![("awl_hello", "greet", None), ("awl_hello", "shout", None)]
    );
    Ok(())
}

/// Obligation 4: a call-site `node` override yields the override in the
/// requirement derivation, AND the document now compiles — direct lowering
/// applies the per-key site-over-declaration config merge
/// (`lower/activity.rs::apply_action_config`).
#[test]
fn call_site_node_override_yields_the_override() -> TestResult {
    let source = "\
//! Fetch a report from the pinned edge host, then hand it back.
workflow pinned_fetch
  input url: String
  outcome fetched: type Report, route success

type Report { body: String }

worker fetcher
  action fetch(url: String) -> Report

step fetch_report
  fetch(url: url) -> report
    node edge01

step handoff
  report |> route fetched
";
    let document = aion_awl::parse(source)?;
    let errors = aion_awl::check_in(&document, Path::new("."));
    assert!(errors.is_empty(), "override fixture must check: {errors:?}");

    let rows: Vec<(String, String, Option<String>)> = action_requirements(&document)
        .into_iter()
        .map(|req| (req.task_queue, req.action, req.node))
        .collect();
    assert_eq!(
        rows,
        vec![(
            "fetcher".to_owned(),
            "fetch".to_owned(),
            Some("edge01".to_owned())
        )]
    );

    let compiled = compile(source, Path::new(".")).map_err(|error| error.to_string())?;
    assert!(
        !compiled.beam_bytes.is_empty(),
        "the pinned-fetch document must compile to a non-empty BEAM artifact"
    );
    Ok(())
}

/// A declared `node` with mixed call sites: the unpinned pipe stage keeps
/// the declared requirement while the overriding call adds its own row.
#[test]
fn declared_node_and_override_both_surface() -> TestResult {
    let source = "\
//! Scan a path twice: once wherever the queue lands, once pinned.
workflow scan_twice
  input path: String
  outcome done: type Scan, route success

type Scan { hits: Int }

worker scanner
  action scan(path: String) -> Scan
    node default_host

step first_pass
  scan(path: path) -> early
    node pinned_host

step second_pass
  path |> scan |> route done
";
    let document = aion_awl::parse(source)?;
    let errors = aion_awl::check_in(&document, Path::new("."));
    assert!(errors.is_empty(), "mixed fixture must check: {errors:?}");

    let rows: Vec<Option<String>> = action_requirements(&document)
        .into_iter()
        .map(|req| req.node)
        .collect();
    assert_eq!(
        rows,
        vec![
            Some("pinned_host".to_owned()),
            Some("default_host".to_owned())
        ]
    );
    Ok(())
}

/// Obligation 5: a lowering refusal passes through with the same
/// span-anchored unsupported diagnostic the MIR path produces today —
/// structured fields and rendered prose both identical.
#[test]
fn refusal_passthrough_is_lossless() -> TestResult {
    let (source, root) = read(&fixture("loop-outcomes/valid/on_failure_compensation.awl"))?;
    let document = aion_awl::parse(&source)?;
    let Err(mir_error) = lower(&document, Some(&root)) else {
        return Err("on_failure_compensation now lowers; pick a refused fixture".into());
    };
    let LowerError::Unsupported { shape, span } = &mir_error else {
        return Err(format!("expected an unsupported refusal, got {mir_error:?}").into());
    };

    match compile(&source, &root) {
        Err(error) => {
            assert_eq!(error.to_string(), mir_error.to_string());
            match error {
                CompileError::Unsupported {
                    shape: compiled_shape,
                    span: compiled_span,
                } => {
                    assert_eq!(&compiled_shape, shape);
                    assert_eq!(&compiled_span, span);
                }
                other => {
                    return Err(format!("expected the MIR path's refusal, got {other:?}").into());
                }
            }
        }
        Ok(_) => return Err("on_failure_compensation unexpectedly compiled".into()),
    }
    Ok(())
}

/// Obligation 6: a language-error fixture surfaces exactly the diagnostics
/// `check_in` (the CLI's check/emit path) produces.
#[test]
fn language_errors_pass_through_verbatim() -> TestResult {
    let (source, root) = read(&fixture("loop-outcomes/invalid/when_without_otherwise.awl"))?;
    let document = aion_awl::parse(&source)?;
    let expected = aion_awl::check_in(&document, &root);
    assert!(!expected.is_empty(), "invalid fixture no longer errors");

    match compile(&source, &root) {
        Err(CompileError::Check(errors)) => assert_eq!(errors, expected),
        other => return Err(format!("expected the check diagnostics, got {other:?}").into()),
    }
    Ok(())
}

/// The new envelope derivation over multiple outcomes: one `oneOf` arm per
/// declared outcome, each pairing the outcome `const` with its payload.
#[test]
fn multi_outcome_envelope_derives_one_arm_per_outcome() -> TestResult {
    let (source, root) = read(&fixture("loop-outcomes/valid/guard_optional_wait.awl"))?;
    let document = aion_awl::parse(&source)?;
    let envelope = aion_awl::schema_for_outcomes_in(&document, &root)?;

    let arms = envelope
        .get("oneOf")
        .and_then(|value| value.as_array())
        .ok_or("multi-outcome envelope has no oneOf")?;
    let names: Vec<&str> = arms
        .iter()
        .filter_map(|arm| arm.pointer("/properties/outcome/const")?.as_str())
        .collect();
    assert_eq!(names, vec!["confirmed", "timed_out"]);
    for arm in arms {
        assert!(
            arm.pointer("/properties/payload/type").is_some(),
            "arm payload is not an inlined schema: {arm}"
        );
    }
    assert_eq!(
        envelope.get("required"),
        Some(&serde_json::json!(["outcome", "payload"]))
    );
    Ok(())
}

/// Walk a schema tree for a bare document-root `{"$ref": "#"}` — inside the
/// envelope such a ref would resolve to the envelope, not the payload type.
fn has_bare_root_ref(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => object.iter().any(|(key, inner)| {
            (key == "$ref" && inner.as_str() == Some("#")) || has_bare_root_ref(inner)
        }),
        serde_json::Value::Array(items) => items.iter().any(has_bare_root_ref),
        _ => false,
    }
}

/// A self-recursive payload type cannot inline (its `"#"` self-reference
/// would re-anchor to the envelope root), so the derivation falls back to
/// `$defs`: the payload becomes a `#/$defs` ref and every internal ref
/// resolves at the envelope root.
#[test]
fn recursive_payload_derives_through_defs_not_inline() -> TestResult {
    let source = "\
//! Chart a directory tree and hand the whole structure back.
workflow tree_chart
  input path: String
  outcome charted: type Tree, route success

type Tree { value: Int, children: [Tree] }

worker walker
  action walk(path: String) -> Tree

step walk_root
  walk(path: path) -> tree

step handoff
  tree |> route charted
";
    let document = aion_awl::parse(source)?;
    let errors = aion_awl::check_in(&document, Path::new("."));
    assert!(
        errors.is_empty(),
        "recursive fixture must check: {errors:?}"
    );

    let envelope = aion_awl::schema_for_outcomes_in(&document, Path::new("."))?;
    assert_eq!(
        envelope.pointer("/properties/payload"),
        Some(&serde_json::json!({ "$ref": "#/$defs/Tree" })),
        "recursive payload was not routed through $defs: {envelope}"
    );
    assert_eq!(
        envelope.pointer("/$defs/Tree/properties/children/items"),
        Some(&serde_json::json!({ "$ref": "#/$defs/Tree" })),
        "self-reference does not resolve at the envelope root: {envelope}"
    );
    assert!(
        !has_bare_root_ref(&envelope),
        "envelope leaked a bare root ref: {envelope}"
    );
    Ok(())
}

/// Mutual recursion trips the fallback through the trial-`$defs` guard: the
/// inlined body itself carries no `"#"`, but a trial def does, so the
/// derivation must still fall back rather than ship a mis-anchored ref.
#[test]
fn mutually_recursive_payload_falls_back_via_trial_defs() -> TestResult {
    let source = "\
//! Report a thread of replies, each reply carrying its thread.
workflow thread_report
  input topic: String
  outcome reported: type Thread, route success

type Thread { topic: String, replies: [Reply] }
type Reply  { body: String, thread: Thread }

worker forum
  action fetch(topic: String) -> Thread

step fetch_thread
  fetch(topic: topic) -> thread

step handoff
  thread |> route reported
";
    let document = aion_awl::parse(source)?;
    let errors = aion_awl::check_in(&document, Path::new("."));
    assert!(errors.is_empty(), "mutual fixture must check: {errors:?}");

    let envelope = aion_awl::schema_for_outcomes_in(&document, Path::new("."))?;
    assert_eq!(
        envelope.pointer("/properties/payload"),
        Some(&serde_json::json!({ "$ref": "#/$defs/Thread" })),
        "mutually recursive payload was not routed through $defs: {envelope}"
    );
    assert_eq!(
        envelope.pointer("/$defs/Reply/properties/thread"),
        Some(&serde_json::json!({ "$ref": "#/$defs/Thread" })),
        "back-reference does not resolve at the envelope root: {envelope}"
    );
    assert!(
        envelope.pointer("/$defs/Thread").is_some(),
        "Thread def missing from the envelope: {envelope}"
    );
    assert!(
        !has_bare_root_ref(&envelope),
        "envelope leaked a bare root ref: {envelope}"
    );
    Ok(())
}

/// The replicated Display prose for the two remaining lowering variants
/// renders exactly as `LowerError` does, so drift on either side fails here
/// (the `Unsupported` variant is pinned live by the refusal passthrough).
#[test]
fn lowering_display_parity_for_message_and_planning() {
    let span = Span {
        start: 12,
        end: 15,
        line: 4,
        column: 3,
    };
    let message = "loop bound must be a literal".to_owned();
    assert_eq!(
        CompileError::Lower {
            message: message.clone(),
            span,
        }
        .to_string(),
        LowerError::Message {
            message: message.clone(),
            span,
        }
        .to_string(),
    );
    let planning = "emitter planning refused the document".to_owned();
    assert_eq!(
        CompileError::Planning {
            message: planning.clone(),
        }
        .to_string(),
        LowerError::Planning { message: planning }.to_string(),
    );
}
