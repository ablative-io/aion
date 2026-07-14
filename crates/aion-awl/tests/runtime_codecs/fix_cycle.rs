//! Flagship fix-cycle execution pin (production runs fb207858/06fce9e5,
//! 2026-07-14): the DIRECT-compiled `dev_brief` died at its first fix-cycle
//! iteration with `{case_clause, <the whole brief record>}` raised inside
//! `developer_input_to_json` — the step's `RoundState(verdicts: [], …)` seed
//! captured whatever X0 last held (the brief) because the `ListNew []`
//! emission materialized nothing before storing. The reference emitter ran
//! the same document's path clean, pinning the direct lowering as the
//! divergence.
//!
//! Two pins: the mechanism pin proves an empty list literal materializes
//! nil at runtime with X0 deliberately poisoned first, and the flagship pin
//! executes `dev_brief`'s own `step_fix_cycle` far enough that the developer
//! input survives its codec and reaches a refusing dispatch stub, with
//! reference-emitter parity on the terminal error.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use aion_awl::mir::{
    Block, FlowFn, FnOrigin, FnRef, MirFn, MirModule, Span, Tail, TyDesc, Value, lower, select,
};
use aion_awl::parse;

use super::drivers::{Body, atom_ref, fn_by_name, lit_ref, push_driver};
use super::harness::{
    build_vm, dispatch_refused_ebin, gleam_build, manifest_dir, reference_module_at,
};

type TestResult = Result<(), Box<dyn Error>>;

fn sp() -> Span {
    Span { line: 0, column: 0 }
}

fn dev_brief_path() -> Result<PathBuf, Box<dyn Error>> {
    Ok(manifest_dir()
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot resolve the repository root")?
        .join("examples/dev-brief/awl/dev_brief.awl"))
}

/// The reference-side driver: seed the round loop exactly as the document's
/// fix-cycle step does and run one dispatch into the refusing stub.
const REF_FIX_CYCLE_DRIVER: &str = r#"
pub fn awl_rt_fix_cycle() {
  let brief = Brief(id: "scratch-first-drive", title: "t", objective: "o", context: "c", pointers: [], scope_in: [], scope_out: [], acceptance: [], notes: "n")
  let config = RunConfig(repo_root: "r", base_branch: "b", gates: [], verify_gates: [], max_fix_cycles: 2, lenses: [])
  let workspace = WorkspaceInfo(workspace_path: "w", branch: "br", base_commit: "bc")
  fix_cycle_loop_0(RoundState(report: None, gate: None, verdicts: [], evidence: ""), 0, config.max_fix_cycles, brief, config, workspace)
}
"#;

/// The mechanism: `[]` must be nil no matter what X0 happens to hold. The
/// marker record leaves a non-nil value in X0 right before the empty literal
/// builds — the pre-fix emission stored exactly that leftover as the "list".
#[test]
fn empty_list_literal_materializes_nil_at_runtime() -> TestResult {
    let mut module = MirModule {
        name: "awl_rt_empty_list".to_owned(),
        source: "awl_rt_empty_list.awl".to_owned(),
        atoms: Vec::new(),
        literals: Vec::new(),
        exports: vec![FnRef(0)],
        functions: Vec::new(),
        types: Vec::new(),
    };
    let marker = atom_ref(&mut module, "marker");
    let mut body = Body::new();
    let _poison = body.record(marker, vec![Value::Int(7)]);
    let empty = body.list(Vec::new());
    module.functions.push(MirFn::Flow(FlowFn {
        origin: FnOrigin::Execute,
        name: "empty_literal".to_owned(),
        params: Vec::new(),
        param_tys: Vec::new(),
        ret_ty: TyDesc::Dynamic,
        body: Block {
            stmts: body.stmts,
            tail: Tail::Return(Value::Var(empty)),
        },
        span: sp(),
        degraded_parallel: false,
    }));
    let bytes = select(&module)?;
    let vm = build_vm(&[], &[bytes])?;
    let formatted = vm.call0("awl_rt_empty_list", "empty_literal")?;
    assert_eq!(
        formatted, "[]",
        "an empty list literal must be nil, not the leftover X0 marker"
    );
    Ok(())
}

/// The flagship path: `dev_brief`'s own `step_fix_cycle` builds the seed
/// (containing the document's `[]` literal), enters the loop, and JSON-encodes
/// the developer input. Reaching the refusing dispatch stub — a clean
/// `{error, _}` instead of the production `case_clause` crash — proves the
/// encode, and the terminal error must match the reference emitter's byte for
/// byte.
#[test]
fn dev_brief_fix_cycle_encode_reaches_dispatch_with_reference_parity() -> TestResult {
    let path = dev_brief_path()?;
    let reference = reference_module_at(&path, REF_FIX_CYCLE_DRIVER)?;
    let mut ebins = gleam_build(&[("ref_dev_brief", &reference)])?;
    ebins.push(dispatch_refused_ebin()?);

    let source = fs::read_to_string(&path)?;
    let document = parse(&source)?;
    let mut module = lower(&document, path.parent())?;
    let step = fn_by_name(&module, "step_fix_cycle")?;
    let brief_tag = atom_ref(&mut module, "brief");
    let config_tag = atom_ref(&mut module, "run_config");
    let workspace_tag = atom_ref(&mut module, "workspace_info");
    let id = lit_ref(&mut module, "scratch-first-drive");
    let title = lit_ref(&mut module, "t");
    let objective = lit_ref(&mut module, "o");
    let context = lit_ref(&mut module, "c");
    let notes = lit_ref(&mut module, "n");
    let repo_root = lit_ref(&mut module, "r");
    let base_branch = lit_ref(&mut module, "b");
    let workspace_path = lit_ref(&mut module, "w");
    let branch = lit_ref(&mut module, "br");
    let base_commit = lit_ref(&mut module, "bc");
    let mut body = Body::new();
    let brief = body.record(
        brief_tag,
        vec![
            Value::Lit(id),
            Value::Lit(title),
            Value::Lit(objective),
            Value::Lit(context),
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Nil,
            Value::Lit(notes),
        ],
    );
    let config = body.record(
        config_tag,
        vec![
            Value::Lit(repo_root),
            Value::Lit(base_branch),
            Value::Nil,
            Value::Nil,
            Value::Int(2),
            Value::Nil,
        ],
    );
    let workspace = body.record(
        workspace_tag,
        vec![
            Value::Lit(workspace_path),
            Value::Lit(branch),
            Value::Lit(base_commit),
        ],
    );
    let result = body.call_local(
        step,
        vec![Value::Var(brief), Value::Var(config), Value::Var(workspace)],
    );
    push_driver(
        &mut module,
        "awl$rt_fix_cycle_step",
        Block {
            stmts: body.stmts,
            tail: Tail::Return(Value::Var(result)),
        },
    );
    let bytes = select(&module)?;

    let vm = build_vm(&ebins, &[bytes])?;
    let direct = vm.call0("dev_brief", "awl$rt_fix_cycle_step")?;
    let reference_result = vm.call0("ref_dev_brief", "awl_rt_fix_cycle")?;
    assert!(
        direct.contains("awl_activity_failed"),
        "the fix cycle must reach the refusing dispatch stub (developer input survived \
         its codec): {direct}"
    );
    assert_eq!(
        direct, reference_result,
        "direct/reference terminal-error parity"
    );
    Ok(())
}
