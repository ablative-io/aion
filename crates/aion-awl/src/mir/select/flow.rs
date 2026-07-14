//! Lower a MIR [`FlowFn`] to the resolved selection [`Body`] (AWL-BC-IR.md
//! §11.4). Atoms, literals, imports, and `FunT` lambdas are interned into the
//! module pools here; the covered op/tail subset (the shapes the checking
//! fixtures actually produce) lowers exactly, and every other §2.5 row is an
//! honest span-anchored `Unsupported` refusal (D-BC3).

use crate::mir::runtime::RuntimeFn;
use crate::mir::{Block, FlowFn, FnRef, MirModule, Span, Stmt, Tail, Test, ToJsonRef, Value};

use super::builder::Builder;
use super::error::SelectError;
use super::ir::{Body, BranchBlock, JsonPair, Src, Step, TailKind, TestKind, Via};

pub(super) fn lower_flow(
    builder: &mut Builder<'_>,
    flow: &FlowFn,
    reference: FnRef,
) -> Result<Body, SelectError> {
    let module_atom = builder.atom(&builder.module.name.clone());
    let name_atom = builder.atom(&flow.name.clone());
    let arity = u8::try_from(flow.param_tys.len()).map_err(|_| SelectError::OutOfRange {
        what: format!("`{}` arity exceeds 255", flow.name),
    })?;
    let labels = Builder::fn_labels(reference);

    let body = lower_block(builder, &flow.body)?;

    Ok(Body {
        params: flow.params.clone(),
        steps: body.steps,
        tail: body.tail,
        name: name_atom,
        module: module_atom,
        arity,
        entry_label: labels.entry,
        code_label: labels.body,
    })
}

fn lower_block(builder: &mut Builder<'_>, block: &Block) -> Result<BranchBlock, SelectError> {
    let mut steps = Vec::with_capacity(block.stmts.len());
    for stmt in &block.stmts {
        steps.push(lower_stmt(builder, stmt)?);
    }
    Ok(BranchBlock {
        steps,
        tail: lower_tail(builder, &block.tail)?,
    })
}

fn src(builder: &mut Builder<'_>, value: &Value) -> Result<Src, SelectError> {
    Ok(match value {
        Value::Var(var) => Src::Var(*var),
        Value::Lit(reference) => Src::Lit(builder.mir_literal(*reference)?),
        Value::Atom(reference) => Src::Atom(builder.mir_atom(reference.0)?),
        Value::Int(value) => Src::Int(*value),
        Value::Nil => Src::Nil,
    })
}

fn srcs(builder: &mut Builder<'_>, values: &[Value]) -> Result<Vec<Src>, SelectError> {
    values.iter().map(|value| src(builder, value)).collect()
}

fn require_var(value: &Value, what: &str, span: Span) -> Result<crate::mir::Var, SelectError> {
    match value {
        Value::Var(var) => Ok(*var),
        _ => Err(SelectError::unsupported(
            format!("{what} over a non-var base"),
            span,
        )),
    }
}

/// The `(arity, code_label)` of a module-local call target.
fn local_ref(module: &MirModule, reference: FnRef) -> Result<(u8, u32), SelectError> {
    let function = module
        .function(reference)
        .ok_or_else(|| SelectError::invariant(format!("fn ref {} out of range", reference.0)))?;
    let arity = u8::try_from(MirModule::arity(function)).map_err(|_| SelectError::OutOfRange {
        what: "local call arity".to_owned(),
    })?;
    Ok((arity, Builder::fn_labels(reference).body))
}

fn lower_stmt(builder: &mut Builder<'_>, stmt: &Stmt) -> Result<Step, SelectError> {
    if let Some(step) = lower_data_stmt(builder, stmt)? {
        return Ok(step);
    }
    match stmt {
        Stmt::CallRt {
            dst, callee, args, ..
        } => {
            let import = builder.import(*callee)?;
            let arity = call_arity(*callee)?;
            Ok(Step::CallImport {
                dst: *dst,
                import,
                arity,
                args: srcs(builder, args)?,
            })
        }
        Stmt::CallLocal {
            dst, callee, args, ..
        } => {
            let (arity, label) = local_ref(builder.module, *callee)?;
            Ok(Step::CallLocal {
                dst: *dst,
                label,
                arity,
                args: srcs(builder, args)?,
            })
        }
        Stmt::MakeClosure {
            dst,
            lifted,
            captures,
            ..
        } => lower_make_closure(builder, *dst, *lifted, captures),
        Stmt::CallClosure { dst, fun, args, .. } => Ok(Step::CallFun {
            dst: *dst,
            fun: src(builder, fun)?,
            args: srcs(builder, args)?,
        }),
        Stmt::TryBind { dst, result, .. } => Ok(Step::TryBind {
            dst: *dst,
            result: *result,
            ok_atom: builder.atom("ok"),
        }),
        Stmt::JsonObj { dst, pairs, .. } => lower_json_obj(builder, *dst, pairs),
        Stmt::Cmp {
            dst, op, lhs, rhs, ..
        } => Ok(Step::Cmp {
            dst: *dst,
            op: *op,
            lhs: src(builder, lhs)?,
            rhs: src(builder, rhs)?,
        }),
        Stmt::BoolOp {
            dst, op, lhs, rhs, ..
        } => Ok(Step::BoolOp {
            dst: *dst,
            op: *op,
            lhs: src(builder, lhs)?,
            rhs: src(builder, rhs)?,
        }),
        Stmt::Not {
            dst, src: value, ..
        } => Ok(Step::Not {
            dst: *dst,
            src: src(builder, value)?,
        }),
        other => Err(unsupported_stmt(other)),
    }
}

/// The pure data-construction/destructure ops (no pools beyond atoms):
/// `Some(step)` when `stmt` is one of them, `None` to fall through to the
/// call/closure/json arms.
fn lower_data_stmt(builder: &mut Builder<'_>, stmt: &Stmt) -> Result<Option<Step>, SelectError> {
    Ok(Some(match stmt {
        Stmt::FieldGet {
            dst,
            base,
            index,
            span,
        } => Step::FieldGet {
            dst: *dst,
            base: require_var(base, "field access", *span)?,
            index: *index,
        },
        Stmt::AssertSome { dst, option, .. } => Step::AssertSome {
            dst: *dst,
            option: *option,
            some_atom: builder.atom("some"),
        },
        Stmt::RecordNew { dst, tag, args, .. } => {
            let tag = builder.mir_atom(tag.0)?;
            let args = srcs(builder, args)?;
            Step::Record {
                dst: *dst,
                tag,
                args,
            }
        }
        Stmt::TupleNew { dst, items, .. } => Step::Tuple {
            dst: *dst,
            items: srcs(builder, items)?,
        },
        Stmt::Increment { dst, src, .. } => Step::Increment {
            dst: *dst,
            src: *src,
        },
        Stmt::ListNew { dst, items, .. } => Step::ListNew {
            dst: *dst,
            items: srcs(builder, items)?,
        },
        Stmt::ListPrepend {
            dst, head, tail, ..
        } => Step::Cons {
            dst: *dst,
            head: src(builder, head)?,
            tail: src(builder, tail)?,
        },
        Stmt::AssertList { binds, list, .. } => Step::AssertList {
            binds: binds.clone(),
            list: *list,
        },
        _ => return Ok(None),
    }))
}

fn lower_make_closure(
    builder: &mut Builder<'_>,
    dst: crate::mir::Var,
    lifted: FnRef,
    captures: &[Value],
) -> Result<Step, SelectError> {
    let function = builder
        .module
        .function(lifted)
        .ok_or_else(|| SelectError::invariant("make_closure target out of range"))?;
    let name = function.name().to_owned();
    let physical = MirModule::arity(function);
    let free = u32::try_from(captures.len()).map_err(|_| SelectError::OutOfRange {
        what: "capture count".to_owned(),
    })?;
    // beamr's `make_fun2` convention: the `FunT` arity is the CALLABLE arity
    // (declared args), with the `num_free` captures marshaled from `x0..` at
    // creation and appended after the args at call time.
    let declared = physical
        .checked_sub(free)
        .ok_or_else(|| SelectError::invariant("closure captures exceed physical arity"))?;
    let arity = u8::try_from(declared).map_err(|_| SelectError::OutOfRange {
        what: "closure arity".to_owned(),
    })?;
    let name_atom = builder.atom(&name);
    let code_label = Builder::fn_labels(lifted).body;
    let lambda = builder.lambda(name_atom, arity, code_label, free);
    Ok(Step::MakeClosure {
        dst,
        lambda,
        captures: srcs(builder, captures)?,
    })
}

fn lower_json_obj(
    builder: &mut Builder<'_>,
    dst: crate::mir::Var,
    pairs: &[(String, crate::mir::JsonVal)],
) -> Result<Step, SelectError> {
    let object_import = builder.import(RuntimeFn::JObject)?;
    let mut lowered = Vec::with_capacity(pairs.len());
    for (name, json) in pairs {
        let crate::mir::JsonVal::Encoded { value, via } = json;
        let name_lit = builder.binary_literal(name.clone().into_bytes());
        let via = match via {
            ToJsonRef::SdkLeaf(leaf) => Via::Import(builder.import(RuntimeFn::LeafToJson(*leaf))?),
            ToJsonRef::Local(reference) => Via::Local(Builder::fn_labels(*reference).body),
        };
        lowered.push(JsonPair {
            name_lit,
            value: src(builder, value)?,
            via,
        });
    }
    Ok(Step::JsonObj {
        dst,
        pairs: lowered,
        object_import,
    })
}

fn lower_tail(builder: &mut Builder<'_>, tail: &Tail) -> Result<TailKind, SelectError> {
    match tail {
        Tail::Return(value) => Ok(TailKind::Return(src(builder, value)?)),
        Tail::TailRt { callee, args } => Ok(TailKind::TailImport {
            import: builder.import(*callee)?,
            arity: call_arity(*callee)?,
            args: srcs(builder, args)?,
        }),
        Tail::TailLocal { callee, args } => {
            let (arity, label) = local_ref(builder.module, *callee)?;
            Ok(TailKind::TailLocal {
                label,
                arity,
                args: srcs(builder, args)?,
            })
        }
        Tail::If {
            test,
            then_block,
            else_block,
            ..
        } => Ok(TailKind::If {
            test: lower_test(builder, test)?,
            then_block: Box::new(lower_block(builder, then_block)?),
            else_block: Box::new(lower_block(builder, else_block)?),
        }),
        Tail::SelectEnum { subject, arms, .. } => {
            let subject = src(builder, subject)?;
            let mut lowered = Vec::with_capacity(arms.len());
            for (tag, block) in arms {
                lowered.push((builder.mir_atom(tag.0)?, lower_block(builder, block)?));
            }
            Ok(TailKind::SelectEnum {
                subject,
                arms: lowered,
            })
        }
    }
}

fn lower_test(builder: &mut Builder<'_>, test: &Test) -> Result<TestKind, SelectError> {
    Ok(match test {
        Test::IsTrue(value) => TestKind::IsTrue(src(builder, value)?),
        Test::Cmp { op, lhs, rhs } => TestKind::Cmp {
            op: *op,
            lhs: src(builder, lhs)?,
            rhs: src(builder, rhs)?,
        },
        Test::IsTagged { value, tag, arity } => TestKind::IsTagged {
            value: src(builder, value)?,
            tag: builder.mir_atom(tag.0)?,
            arity: *arity,
        },
        Test::Not(inner) => TestKind::Not(Box::new(lower_test(builder, inner)?)),
    })
}

fn call_arity(callee: RuntimeFn) -> Result<u8, SelectError> {
    u8::try_from(callee.signature().2).map_err(|_| SelectError::OutOfRange {
        what: format!("runtime call arity for {callee:?}"),
    })
}

fn unsupported_stmt(stmt: &Stmt) -> SelectError {
    let (what, span) = match stmt {
        Stmt::Bind { span, .. } => ("bind", *span),
        Stmt::WaitTimeoutCase { span, .. } => ("wait_timeout", *span),
        Stmt::Cmp { span, .. } => ("cmp", *span),
        Stmt::BoolOp { span, .. } => ("boolop", *span),
        Stmt::Not { span, .. } => ("not", *span),
        Stmt::Concat { span, .. } => ("concat", *span),
        Stmt::AssertSome { span, .. } => ("assert_some", *span),
        Stmt::IndexGuard { span, .. } => ("index_guard", *span),
        Stmt::Attempt { span, .. } => ("attempt", *span),
        _ => ("op", Span::zero()),
    };
    SelectError::unsupported(what, span)
}
