//! `verify(&MirModule)` — the S1 pass that runs inside every MIR golden test.
//!
//! Checks reachable from `&MirModule` alone: capability closure (the closed
//! `RuntimeFn` manifest — the `ResultTry` fallback is never minted by the
//! primary design — and every runtime call's arity against the fixed
//! `RuntimeFn::signature` table, §6/IR-18), local- and runtime-call arity,
//! single-def variable discipline, atom/literal/function reference resolution,
//! tail invariants (structurally guaranteed — each `Block` ends in exactly one
//! `Tail`, control constructs are tails), and the export set.
//!
//! The per-op result-type cross-check against the rev-2 `TypeEnv` that S1 also
//! names is NOT reachable from `verify(&MirModule)` — the environment is not
//! carried by this signature. It is performed in BC-3, which holds the
//! `TypeEnv` during instruction selection; this split is recorded in
//! `AWL-BC-IR.md` §7 (verifier scope) rather than deferred silently.

use std::collections::BTreeSet;

use super::func::MirFn;
use super::ids::{AtomRef, FnRef, LitRef, Var};
use super::ops::{Block, JsonVal, Stmt, Tail, Test, Value};
use super::runtime::RuntimeFn;
use super::unit::MirModule;

/// A verification failure, anchored to the offending function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    pub function: String,
    pub message: String,
}

impl VerifyError {
    fn new(function: &str, message: impl Into<String>) -> Self {
        Self {
            function: function.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "verify: {} in {}", self.message, self.function)
    }
}

impl std::error::Error for VerifyError {}

/// Verify a lowered module. Runs under every golden test (§9).
pub fn verify(module: &MirModule) -> Result<(), VerifyError> {
    verify_exports(module)?;
    for function in &module.functions {
        if let MirFn::Flow(flow) = function {
            let mut defined: BTreeSet<Var> = flow.params.iter().copied().collect();
            if defined.len() != flow.params.len() {
                return Err(VerifyError::new(&flow.name, "duplicate parameter var"));
            }
            if flow.params.len() != flow.param_tys.len() {
                return Err(VerifyError::new(
                    &flow.name,
                    "param count does not match param_tys",
                ));
            }
            verify_block(module, &flow.name, &flow.body, &mut defined)?;
        }
    }
    Ok(())
}

fn verify_exports(module: &MirModule) -> Result<(), VerifyError> {
    let mut seen = Vec::new();
    for reference in &module.exports {
        let function = module
            .function(*reference)
            .ok_or_else(|| VerifyError::new("<exports>", "export references no function"))?;
        seen.push((function.name().to_owned(), MirModule::arity(function)));
    }
    for (name, arity) in [("run", 1_u32), ("definition", 0), ("execute", 1)] {
        if !seen.iter().any(|(n, a)| n == name && *a == arity) {
            return Err(VerifyError::new(
                "<exports>",
                format!("export set is missing {name}/{arity}"),
            ));
        }
    }
    if seen.len() != 3 {
        return Err(VerifyError::new(
            "<exports>",
            "export set must be exactly run/1, definition/0, execute/1",
        ));
    }
    Ok(())
}

fn verify_block(
    module: &MirModule,
    function: &str,
    block: &Block,
    defined: &mut BTreeSet<Var>,
) -> Result<(), VerifyError> {
    for stmt in &block.stmts {
        verify_stmt(module, function, stmt, defined)?;
    }
    verify_tail(module, function, &block.tail, defined)
}

fn verify_stmt(
    module: &MirModule,
    function: &str,
    stmt: &Stmt,
    defined: &mut BTreeSet<Var>,
) -> Result<(), VerifyError> {
    if let Some(callee) = stmt.runtime_callee() {
        check_capability(function, callee)?;
    }
    match stmt {
        Stmt::CallRt { callee, args, .. } => {
            check_runtime_arity(function, *callee, args.len())?;
        }
        Stmt::CallLocal { callee, args, .. } => {
            check_local_arity(module, function, *callee, args.len())?;
        }
        Stmt::MakeClosure { lifted, .. }
        | Stmt::WaitTimeoutCase {
            receive: lifted, ..
        } => {
            check_fn(module, function, *lifted)?;
        }
        Stmt::JsonObj { pairs, .. } => {
            for (_, JsonVal::Encoded { value, .. }) in pairs {
                check_value(module, function, value)?;
            }
        }
        Stmt::Attempt {
            lifted,
            captures,
            on_ok,
            on_err,
            ..
        } => {
            check_fn(module, function, *lifted)?;
            for value in captures {
                check_value(module, function, value)?;
            }
            // Sub-blocks share the enclosing single-def frame.
            verify_block(module, function, on_ok, defined)?;
            verify_block(module, function, on_err, defined)?;
        }
        _ => {}
    }
    for value in stmt_values(stmt) {
        check_value(module, function, &value)?;
    }
    if let Some(dst) = stmt.defined() {
        if !defined.insert(dst) {
            return Err(VerifyError::new(
                function,
                format!("variable v{} defined more than once", dst.0),
            ));
        }
    }
    if let Stmt::AssertList { binds, .. } = stmt {
        for bind in binds.iter().flatten() {
            if !defined.insert(*bind) {
                return Err(VerifyError::new(
                    function,
                    format!("variable v{} defined more than once", bind.0),
                ));
            }
        }
    }
    Ok(())
}

fn verify_tail(
    module: &MirModule,
    function: &str,
    tail: &Tail,
    defined: &mut BTreeSet<Var>,
) -> Result<(), VerifyError> {
    match tail {
        Tail::Return(value) => check_value(module, function, value),
        Tail::TailLocal { callee, args } => {
            check_local_arity(module, function, *callee, args.len())?;
            for value in args {
                check_value(module, function, value)?;
            }
            Ok(())
        }
        Tail::TailRt { callee, args } => {
            check_capability(function, *callee)?;
            check_runtime_arity(function, *callee, args.len())?;
            for value in args {
                check_value(module, function, value)?;
            }
            Ok(())
        }
        Tail::If {
            test,
            then_block,
            else_block,
            ..
        } => {
            check_test(module, function, test)?;
            let mut then_defined = defined.clone();
            verify_block(module, function, then_block, &mut then_defined)?;
            let mut else_defined = defined.clone();
            verify_block(module, function, else_block, &mut else_defined)
        }
        Tail::SelectEnum { subject, arms, .. } => {
            check_value(module, function, subject)?;
            for (atom, arm) in arms {
                check_atom(module, function, *atom)?;
                let mut arm_defined = defined.clone();
                verify_block(module, function, arm, &mut arm_defined)?;
            }
            Ok(())
        }
    }
}

fn check_capability(function: &str, callee: RuntimeFn) -> Result<(), VerifyError> {
    if matches!(callee, RuntimeFn::ResultTry) {
        return Err(VerifyError::new(
            function,
            "the ResultTry fallback (R1) must not appear in the primary design",
        ));
    }
    if matches!(callee, RuntimeFn::IntAdd) {
        return Err(VerifyError::new(
            function,
            "erlang:'+' is bif-position only (the Increment burst) — never a call target",
        ));
    }
    Ok(())
}

/// Every runtime call's argument count must match the fixed
/// `RuntimeFn::signature` arity (§6/IR-18) — the import table is derived from
/// the instruction stream, so a mismatch would mint a malformed `ImpT` row.
fn check_runtime_arity(function: &str, callee: RuntimeFn, args: usize) -> Result<(), VerifyError> {
    let (_, _, arity) = callee.signature();
    if arity as usize != args {
        return Err(VerifyError::new(
            function,
            format!(
                "runtime call {} expects arity {arity}, got {args}",
                callee.label()
            ),
        ));
    }
    Ok(())
}

fn check_local_arity(
    module: &MirModule,
    function: &str,
    callee: FnRef,
    args: usize,
) -> Result<(), VerifyError> {
    let target = module
        .function(callee)
        .ok_or_else(|| VerifyError::new(function, "local call references no function"))?;
    let arity = MirModule::arity(target) as usize;
    if arity != args {
        return Err(VerifyError::new(
            function,
            format!(
                "local call to {} expects arity {arity}, got {args}",
                target.name()
            ),
        ));
    }
    Ok(())
}

fn check_fn(module: &MirModule, function: &str, reference: FnRef) -> Result<(), VerifyError> {
    if module.function(reference).is_none() {
        return Err(VerifyError::new(
            function,
            "reference resolves to no function",
        ));
    }
    Ok(())
}

fn check_value(module: &MirModule, function: &str, value: &Value) -> Result<(), VerifyError> {
    match value {
        Value::Lit(reference) => check_lit(module, function, *reference),
        Value::Atom(atom) => check_atom(module, function, *atom),
        Value::Var(_) | Value::Int(_) | Value::Nil => Ok(()),
    }
}

fn check_lit(module: &MirModule, function: &str, reference: LitRef) -> Result<(), VerifyError> {
    if (reference.0 as usize) >= module.literals.len() {
        return Err(VerifyError::new(function, "literal reference out of range"));
    }
    Ok(())
}

fn check_atom(module: &MirModule, function: &str, atom: AtomRef) -> Result<(), VerifyError> {
    if module.atom(atom.0).is_none() {
        return Err(VerifyError::new(function, "atom reference out of range"));
    }
    Ok(())
}

fn check_test(module: &MirModule, function: &str, test: &Test) -> Result<(), VerifyError> {
    match test {
        Test::IsTrue(value) => check_value(module, function, value),
        Test::Cmp { lhs, rhs, .. } => {
            check_value(module, function, lhs)?;
            check_value(module, function, rhs)
        }
        Test::IsTagged { value, tag, .. } => {
            check_value(module, function, value)?;
            check_atom(module, function, *tag)
        }
        Test::Not(inner) => check_test(module, function, inner),
    }
}

/// The value operands an op reads (for atom/literal resolution). Captures and
/// nested blocks are handled by the caller.
fn stmt_values(stmt: &Stmt) -> Vec<Value> {
    match stmt {
        Stmt::Bind { value, .. } => vec![value.clone()],
        Stmt::FieldGet { base, .. } => vec![base.clone()],
        Stmt::RecordNew { args, .. }
        | Stmt::TupleNew { items: args, .. }
        | Stmt::ListNew { items: args, .. }
        | Stmt::CallRt { args, .. }
        | Stmt::CallLocal { args, .. } => args.clone(),
        Stmt::CallClosure { fun, args, .. } => {
            let mut values = vec![fun.clone()];
            values.extend(args.iter().cloned());
            values
        }
        Stmt::MakeClosure { captures, .. } => captures.clone(),
        Stmt::Cmp { lhs, rhs, .. }
        | Stmt::BoolOp { lhs, rhs, .. }
        | Stmt::Concat { lhs, rhs, .. }
        | Stmt::ListPrepend {
            head: lhs,
            tail: rhs,
            ..
        } => {
            vec![lhs.clone(), rhs.clone()]
        }
        Stmt::Not { src, .. } => vec![src.clone()],
        Stmt::TryBind { .. }
        | Stmt::WaitTimeoutCase { .. }
        | Stmt::Increment { .. }
        | Stmt::AssertList { .. }
        | Stmt::AssertSome { .. }
        | Stmt::IndexGuard { .. }
        | Stmt::JsonObj { .. }
        | Stmt::Attempt { .. } => Vec::new(),
    }
}
