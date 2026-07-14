//! B-2 proof obligations for the programmatic AWL assembler: determinism,
//! format-v1 admission, SDK closure coverage, bundle integrity, collision
//! refusal, and the dev-time regeneration script's product-path isolation.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use aion_awl::{ActionRequirement, CompiledWorkflow};
use aion_awl_package::{
    AssembleError, AwlAssembleOptions, DEFAULT_WORKFLOW_TIMEOUT, assemble_awl, sdk_closure_modules,
    sdk_closure_version,
};
use aion_package::{ExtractionLimits, Package};
use beamr::atom::AtomTable;
use beamr::loader::load_beam_chunks;
use sha2::{Digest, Sha256};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Pinned digest of the committed SDK closure (names, lengths, bytes, in
/// canonical order). A silent edit to the embedded bundle fails here; a
/// deliberate regeneration updates this pin in the same commit.
const BUNDLE_CHECKSUM: &str = "aecf79296bf8d66d64795d9282ce6f052257926e48c29a681c643273fae62ac2";
const BUNDLE_MODULE_COUNT: usize = 45;
const BUNDLE_SDK_VERSION: &str = "0.6.0";

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.join("../..")
}

fn compile_awl_hello() -> Result<CompiledWorkflow, Box<dyn std::error::Error>> {
    let path = workspace_root().join("examples/awl-hello/awl_hello.awl");
    let source = fs::read_to_string(&path)?;
    let root = path
        .parent()
        .ok_or("awl_hello.awl has no parent directory")?
        .to_path_buf();
    Ok(aion_awl::compile(&source, &root)?)
}

/// Distinct module names a BEAM imports, read through the same beamr chunk
/// loader the runtime rename machinery builds on. An unresolvable import
/// atom is a hard error, never a silent drop — an honesty test that
/// silently shrank its coverage set would pass vacuously.
fn imported_modules(beam_bytes: &[u8]) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let atoms = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(beam_bytes, &atoms)?;
    let mut modules = Vec::new();
    for import in &parsed.imports {
        let Some(name) = atoms.resolve(import.module) else {
            return Err(format!("unresolvable import module atom: {:?}", import.module).into());
        };
        modules.push(name.to_owned());
    }
    modules.sort_unstable();
    modules.dedup();
    Ok(modules)
}

/// Proof 1 — same-process determinism: the same compile output assembles
/// to byte-identical archives whose content hash round-trips through the
/// loader. (This alone cannot catch wall-clock leakage — two calls land in
/// the same zip timestamp window — so cross-process stability is proven
/// separately by the pinned-archive test below.)
#[test]
fn assembling_twice_yields_byte_identical_archives() -> TestResult {
    let compiled = compile_awl_hello()?;
    let first = assemble_awl(&compiled, AwlAssembleOptions::default())?;
    let second = assemble_awl(&compiled, AwlAssembleOptions::default())?;
    assert!(!first.is_empty());
    assert_eq!(first, second);

    let package = Package::load_from_bytes(&first, ExtractionLimits::unbounded())?;
    assert_eq!(
        package.manifest().version.as_str(),
        package.content_hash().to_string()
    );
    Ok(())
}

/// Pinned SHA-256 of the archive assembled from the fixed hand-built
/// compile output below. Unlike the `awl_hello` archive (whose BEAM bytes
/// vary with the cargo feature graph), every input here is a literal, so
/// this pin holds across processes, machines, and time: any wall-clock
/// timestamp, environment read, or ordering instability anywhere in the
/// assembly path changes these bytes and fails this test. A deliberate SDK
/// closure regeneration updates this pin in the same commit, exactly like
/// `BUNDLE_CHECKSUM`.
const FIXED_INPUT_ARCHIVE_SHA256: &str =
    "cd2e692134817e60826d76f5e17895f6fca71f85ef18bb192f51ea51c360f3b3";

/// Proof 1b — cross-process determinism: a fully fixed input assembles to
/// pinned archive bytes.
#[test]
fn fixed_input_archive_bytes_match_the_cross_process_pin() -> TestResult {
    let bytes = assemble_awl(&hand_built("pin_case"), AwlAssembleOptions::default())?;
    assert_eq!(sha256_hex(&bytes)?, FIXED_INPUT_ARCHIVE_SHA256);
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> Result<String, Box<dyn std::error::Error>> {
    let mut digest = Sha256::new();
    digest.update(bytes);
    let mut hex = String::new();
    for byte in digest.finalize() {
        use std::fmt::Write as _;
        write!(hex, "{byte:02x}")?;
    }
    Ok(hex)
}

/// Proof 2 — admission: `Package::load_from_bytes` accepts the archive and
/// every derived manifest field round-trips exactly; the entry module is in
/// the BEAM set with the exact compiled bytes.
#[test]
fn format_v1_loader_admits_the_archive_with_derived_manifest() -> TestResult {
    let compiled = compile_awl_hello()?;
    let bytes = assemble_awl(&compiled, AwlAssembleOptions::default())?;
    let package = Package::load_from_bytes(bytes, ExtractionLimits::unbounded())?;

    let manifest = package.manifest();
    assert_eq!(manifest.entry_module, "awl_hello");
    assert_eq!(manifest.entry_function, "run");
    assert_eq!(manifest.timeout, DEFAULT_WORKFLOW_TIMEOUT);
    assert_eq!(manifest.input_schema, compiled.input_schema);
    assert_eq!(manifest.output_schema, compiled.output_schema);

    // Non-circular legacy parity: the derived manifest schemas equal the
    // schema documents the legacy `package_project` path embeds for this
    // same example — including the `$schema` dialect pin — not merely the
    // compile output they were copied from.
    let schemas = workspace_root().join("examples/awl-hello/schemas");
    let legacy_input: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(schemas.join("input.json"))?)?;
    let legacy_output: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(schemas.join("output.json"))?)?;
    assert_eq!(
        manifest.input_schema, legacy_input,
        "derived input schema drifted from the legacy embedded document"
    );
    assert_eq!(
        manifest.output_schema, legacy_output,
        "derived output schema drifted from the legacy embedded document"
    );
    let activities: Vec<&str> = manifest
        .activities
        .iter()
        .map(|activity| activity.activity_type.as_str())
        .collect();
    assert_eq!(activities, vec!["greet", "shout"]);

    assert_eq!(
        package.beams().get("awl_hello"),
        Some(compiled.beam_bytes.as_slice())
    );
    assert_eq!(package.beams().len(), BUNDLE_MODULE_COUNT + 1);
    assert!(package.source().is_empty());
    Ok(())
}

/// Proof 3 — closure coverage (the honesty test): every module the compiled
/// workflow BEAM imports is shipped in the bundle or is the workflow module
/// itself. A missing import fails here, not as a runtime surprise later.
/// Also pins the name contract the assembler relies on: the BEAM's internal
/// module name equals the AWL workflow name.
#[test]
fn every_workflow_beam_import_is_shipped_in_the_closure() -> TestResult {
    let compiled = compile_awl_hello()?;

    let atoms = AtomTable::with_common_atoms();
    let parsed = load_beam_chunks(&compiled.beam_bytes, &atoms)?;
    assert_eq!(
        atoms.resolve(parsed.name),
        Some(compiled.workflow_name.as_str())
    );

    // Vacuous-pass guard: the coverage set must be non-empty and contain a
    // module the emitter is known to call, or the loop below proves nothing.
    let imports = imported_modules(&compiled.beam_bytes)?;
    assert!(
        !imports.is_empty(),
        "the workflow BEAM reports zero imports — import extraction is broken"
    );
    assert!(
        imports
            .iter()
            .any(|module| module == WORKFLOW_IMPORT_ANCHOR),
        "expected emitter call target `{WORKFLOW_IMPORT_ANCHOR}` among imports: {imports:?}"
    );

    let bundle: BTreeSet<&str> = sdk_closure_modules().map(|(name, _)| name).collect();
    let mut missing = Vec::new();
    for module in imports {
        if module != compiled.workflow_name && !bundle.contains(module.as_str()) {
            missing.push(module);
        }
    }
    assert!(
        missing.is_empty(),
        "workflow BEAM imports missing from the SDK closure: {missing:?}"
    );
    Ok(())
}

/// A module the direct compiler's emitter always calls from a workflow BEAM
/// (its runtime support module) — the named anchor that keeps the
/// import-coverage proofs from passing vacuously.
const WORKFLOW_IMPORT_ANCHOR: &str = "aion@awl@runtime";

/// Modules the engine provides natively (the SDK's NIF/FFI namespace,
/// registered by the runtime itself) — deliberately excluded from the
/// bundle by the same filter the legacy discovery applies.
const ENGINE_PROVIDED_MODULES: &[&str] = &["aion_flow_ffi"];

/// Erlang/OTP modules present on any ERTS the engine runs on (`json` is the
/// OTP 27+ builtin `gleam_json_ffi` targets); never shipped by either
/// packaging path. Kept exhaustive on purpose: a new bundle-external import
/// must be classified here deliberately, not absorbed by a pattern.
const ERTS_PROVIDED_MODULES: &[&str] = &[
    "base64",
    "binary",
    "erlang",
    "io",
    "io_lib",
    "io_lib_format",
    "json",
    "lists",
    "maps",
    "math",
    "rand",
    "string",
    "unicode",
    "uri_string",
];

/// Proof 3b — closure closedness: the bundle itself is closed under
/// imports, modulo the engine-provided FFI namespace and ERTS builtins.
/// This is the transitive guarantee proof 3 cannot give: if a future SDK
/// regeneration misses a package (say `aion_flow` gains a new production
/// dependency the harvest overlooks), its modules surface here as
/// bundle-external imports and this test fails — at regen time, not as an
/// `undef` in a deployed worker.
#[test]
fn bundle_is_closed_under_imports_modulo_engine_and_erts_modules() -> TestResult {
    let bundle: BTreeSet<&str> = sdk_closure_modules().map(|(name, _)| name).collect();

    let mut leaked = BTreeSet::new();
    let mut engine_seen = false;
    for (name, bytes) in sdk_closure_modules() {
        let imports = imported_modules(bytes)?;
        assert!(
            !imports.is_empty(),
            "bundle module `{name}` reports zero imports"
        );
        for import in imports {
            if ENGINE_PROVIDED_MODULES.contains(&import.as_str()) {
                engine_seen = true;
                continue;
            }
            if bundle.contains(import.as_str()) || ERTS_PROVIDED_MODULES.contains(&import.as_str())
            {
                continue;
            }
            leaked.insert(format!("{name} -> {import}"));
        }
    }
    assert!(
        leaked.is_empty(),
        "bundle is not closed under imports (missing modules would be `undef` at runtime): {leaked:?}"
    );
    // The engine-provided whitelist must be live (the SDK really does call
    // its FFI namespace), and no whitelisted module may also be shipped —
    // a stale whitelist entry would quietly widen the escape hatch.
    assert!(
        engine_seen,
        "no bundle module imports the engine FFI namespace"
    );
    for allowed in ENGINE_PROVIDED_MODULES.iter().chain(ERTS_PROVIDED_MODULES) {
        assert!(
            !bundle.contains(allowed),
            "whitelisted module `{allowed}` is also shipped in the bundle"
        );
    }
    Ok(())
}

/// Proof 4 — bundle integrity: non-empty, versioned, carries the `aion_flow`
/// and gleam stdlib modules `awl_hello`'s BEAM actually imports, and matches
/// the pinned checksum so silent bundle edits fail loudly.
#[test]
fn bundle_is_versioned_pinned_and_carries_the_imported_sdk_modules() -> TestResult {
    assert_eq!(sdk_closure_modules().count(), BUNDLE_MODULE_COUNT);
    assert_eq!(sdk_closure_version(), BUNDLE_SDK_VERSION);
    for (_, bytes) in sdk_closure_modules() {
        assert!(!bytes.is_empty());
    }

    let bundle: BTreeSet<&str> = sdk_closure_modules().map(|(name, _)| name).collect();
    let compiled = compile_awl_hello()?;
    for module in imported_modules(&compiled.beam_bytes)? {
        assert!(
            bundle.contains(module.as_str()),
            "imported module `{module}` absent from bundle"
        );
    }

    let mut digest = Sha256::new();
    for (name, bytes) in sdk_closure_modules() {
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    let mut hex = String::new();
    for byte in digest.finalize() {
        use std::fmt::Write as _;
        write!(hex, "{byte:02x}")?;
    }
    assert_eq!(hex, BUNDLE_CHECKSUM);
    Ok(())
}

fn hand_built(name: &str) -> CompiledWorkflow {
    CompiledWorkflow {
        workflow_name: name.to_owned(),
        beam_bytes: b"opaque".to_vec(),
        input_schema: serde_json::json!({ "type": "object" }),
        output_schema: serde_json::json!({ "type": "object" }),
        actions: vec![ActionRequirement {
            task_queue: "q".to_owned(),
            action: "act".to_owned(),
            node: None,
        }],
        sidecar_bytes: Vec::new(),
    }
}

/// Proof 5 — negative: a workflow whose name collides with a bundle module
/// is refused with the dedicated typed error, before any archive is built.
#[test]
fn workflow_name_colliding_with_a_bundle_module_is_refused_typed() {
    let result = assemble_awl(&hand_built("aion_flow"), AwlAssembleOptions::default());
    assert!(matches!(
        result,
        Err(AssembleError::BundleCollision { module }) if module == "aion_flow"
    ));
}

#[test]
fn unsafe_workflow_name_is_refused_typed() {
    let result = assemble_awl(&hand_built("../escape"), AwlAssembleOptions::default());
    assert!(matches!(
        result,
        Err(AssembleError::InvalidWorkflowName { name }) if name == "../escape"
    ));
}

/// Proof 6 — the dev-time regeneration script exists, documents its gleam
/// requirement, and is invoked by no `build.rs` anywhere in the workspace
/// (`aion-awl-package` itself has no build script at all).
#[test]
fn regeneration_script_is_documented_and_outside_the_product_path() -> TestResult {
    let root = workspace_root();
    let script = root.join("scripts/regen-awl-sdk-closure.sh");
    let text = fs::read_to_string(&script)?;
    assert!(text.contains("gleam build"));
    assert!(text.contains("DEV-TIME"));
    assert!(text.contains("PRODUCT path never invokes this script"));

    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("build.rs")
            .exists(),
        "aion-awl-package must not grow a build script"
    );

    let crates_dir = root.join("crates");
    for entry in fs::read_dir(&crates_dir)? {
        let build_rs = entry?.path().join("build.rs");
        if !build_rs.is_file() {
            continue;
        }
        let contents = fs::read_to_string(&build_rs)?;
        assert!(
            !contents.contains("regen-awl-sdk-closure"),
            "{} invokes the dev-time regeneration script",
            build_rs.display()
        );
    }
    Ok(())
}
