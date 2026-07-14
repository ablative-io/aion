//! Named-branch fork lowering (split from `forks` for the 500-line law):
//! source-order activity values in ONE `workflow.all` — typed when every
//! branch calls the same action, raw wire-unified twins + per-position
//! decode when heterogeneous (R5: each bound position decodes with THAT
//! action's return codec and string action name); 0/1 branches fall back to
//! ordinary calls — the reference's exact dispatch
//! (`emitter/forks.rs::lower_named_fork`).

use crate::ast::{CallStmt, ForkStmt, Statement};
use crate::emitter::type_ref_to_g;

use super::super::ids::{Span, Var};
use super::super::ops::{LiveAfter, Stmt, Value};
use super::super::runtime::RuntimeFn;
use super::activity::{activity_value, call_rt};
use super::build::{FnPlan, codec_ref_for};
use super::ctx::Ctx;
use super::driver::LowerError;
use super::expr::{Binding, Scope};

pub(super) fn lower_named_fork(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    fork: &ForkStmt,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(), LowerError> {
    let mut branches: Vec<&CallStmt> = Vec::new();
    for statement in &fork.body {
        match statement {
            Statement::Call(call) if ctx.emitter.actions.contains_key(call.call.name.as_str()) => {
                branches.push(call);
            }
            Statement::Call(call) if ctx.emitter.children.contains_key(call.call.name.as_str()) => {
                // The reference's exact stopgap refusal class.
                return Err(LowerError::unsupported(
                    "child calls inside named fork branches",
                    call.span,
                ));
            }
            _ => {
                return Err(LowerError::unsupported(
                    "a named fork branch beyond an action call",
                    fork.span,
                ));
            }
        }
    }
    let homogeneous = branches.len() > 1
        && branches
            .iter()
            .all(|branch| branch.call.name == branches[0].call.name);
    if branches.len() <= 1 {
        for branch in &branches {
            super::flow::lower_call(ctx, plan, branch, scope, stmts)?;
        }
        return Ok(());
    }
    let span = Span::from_source(fork.span);
    // R4: branch activity values in source order, then the single all-call.
    let raw = !homogeneous;
    let mut values = Vec::new();
    for branch in &branches {
        let queued = activity_value(
            ctx,
            plan,
            &branch.call,
            branch.config.as_ref(),
            None,
            scope,
            stmts,
            raw,
        )?;
        values.push(Value::Var(queued));
    }
    let list = ctx.fresh_var();
    stmts.push(Stmt::ListNew {
        dst: list,
        items: values,
        span,
    });
    let ran = call_rt(
        ctx,
        RuntimeFn::WfAll,
        vec![Value::Var(list)],
        stmts,
        fork.span,
    );
    let mapped = call_rt(
        ctx,
        RuntimeFn::MapActivityError,
        vec![Value::Var(ran)],
        stmts,
        fork.span,
    );
    let joined = ctx.fresh_var();
    stmts.push(Stmt::TryBind {
        dst: joined,
        result: mapped,
        live_after: LiveAfter::default(),
        span,
    });
    // Source-position destructure: `let assert [p0, p1, …] = awl_branches`.
    let binds: Vec<Option<Var>> = branches
        .iter()
        .map(|branch| branch.bind.as_ref().map(|_| ctx.fresh_var()))
        .collect();
    stmts.push(Stmt::AssertList {
        binds: binds.clone(),
        list: joined,
        span,
    });
    bind_branches(ctx, plan, &branches, binds, homogeneous, scope, stmts)?;
    Ok(())
}

/// Insert every bound branch's binding: directly for the typed homogeneous
/// path, through the per-position raw decode for the heterogeneous path.
fn bind_branches(
    ctx: &mut Ctx<'_>,
    plan: &FnPlan,
    branches: &[&CallStmt],
    binds: Vec<Option<Var>>,
    homogeneous: bool,
    scope: &mut Scope,
    stmts: &mut Vec<Stmt>,
) -> Result<(), LowerError> {
    for (branch, bound) in branches.iter().zip(binds) {
        let Some(bind) = &branch.bind else { continue };
        let Some(bound) = bound else { continue };
        let (_, decl) = ctx.emitter.actions[branch.call.name.as_str()];
        let returns = type_ref_to_g(&decl.returns);
        if homogeneous {
            scope.insert(
                bind.name.clone(),
                Binding {
                    var: bound,
                    ty: returns,
                },
            );
            continue;
        }
        // R5: decode this position's raw payload with THIS action's return
        // codec and string action name — wrong-codec decode is the silent
        // failure mode the raw twins exist to prevent.
        let codec_ref = codec_ref_for(ctx, plan, &returns)?;
        let codec = codec_value(ctx, &codec_ref, stmts, branch.call.name_span);
        let name_lit = ctx.binary(&branch.call.name);
        let decoded = call_rt(
            ctx,
            RuntimeFn::Decoded,
            vec![Value::Var(codec), Value::Var(bound), Value::Lit(name_lit)],
            stmts,
            branch.call.name_span,
        );
        let typed = ctx.fresh_var();
        stmts.push(Stmt::TryBind {
            dst: typed,
            result: decoded,
            live_after: LiveAfter::default(),
            span: Span::from_source(branch.call.name_span),
        });
        scope.insert(
            bind.name.clone(),
            Binding {
                var: typed,
                ty: returns,
            },
        );
    }
    Ok(())
}

/// A codec VALUE (0-arity composer call) for a resolved codec reference.
fn codec_value(
    ctx: &mut Ctx<'_>,
    codec: &super::super::func::CodecRef,
    stmts: &mut Vec<Stmt>,
    span: crate::Span,
) -> Var {
    use super::super::func::CodecRef;
    match codec {
        CodecRef::Local(reference) => {
            let dst = ctx.fresh_var();
            stmts.push(Stmt::CallLocal {
                dst: Some(dst),
                callee: *reference,
                args: Vec::new(),
                live_after: LiveAfter::default(),
                span: Span::from_source(span),
            });
            dst
        }
        CodecRef::SdkNil => call_rt(ctx, RuntimeFn::NilCodec, Vec::new(), stmts, span),
        CodecRef::SdkLeaf(leaf) => {
            call_rt(ctx, RuntimeFn::LeafCodec(*leaf), Vec::new(), stmts, span)
        }
    }
}
