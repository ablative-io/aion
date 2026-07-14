//! Direct-side MIR driver construction for the BC-2b-5 runtime codec
//! proof: a tiny straight-line body builder over the public MIR op set, the
//! per-fixture driver functions, and the module-mutation helpers that
//! export them.

use std::error::Error;
use std::fs;

use aion_awl::mir::{
    AtomRef, Block, FlowFn, FnOrigin, FnRef, LitRef, MirFn, MirLiteral, MirModule, RuntimeFn, Span,
    Stmt, Tail, Test, TyDesc, Value, Var, lower,
};
use aion_awl::parse;

use super::harness::fixture;

fn sp() -> Span {
    Span { line: 0, column: 0 }
}

// ---- MIR driver construction --------------------------------------------

/// Intern an atom in the module's MIR atom table (append when absent).
pub(crate) fn atom_ref(module: &mut MirModule, name: &str) -> AtomRef {
    if let Some(index) = module.atoms.iter().position(|atom| atom == name) {
        return AtomRef(u32::try_from(index).unwrap_or(u32::MAX));
    }
    module.atoms.push(name.to_owned());
    AtomRef(u32::try_from(module.atoms.len() - 1).unwrap_or(u32::MAX))
}

/// Append a UTF-8 binary literal to the module's literal pool.
pub(crate) fn lit_ref(module: &mut MirModule, text: &str) -> LitRef {
    module
        .literals
        .push(MirLiteral::Binary(text.as_bytes().to_vec()));
    LitRef(u32::try_from(module.literals.len() - 1).unwrap_or(u32::MAX))
}

/// The `FnRef` of a module function by generated name.
pub(crate) fn fn_by_name(module: &MirModule, name: &str) -> Result<FnRef, Box<dyn Error>> {
    let index = module
        .functions
        .iter()
        .position(|function| match function {
            MirFn::Flow(flow) => flow.name == name,
            MirFn::Templated { name: n, .. } => n == name,
        })
        .ok_or_else(|| format!("module has no function `{name}`"))?;
    Ok(FnRef(u32::try_from(index).unwrap_or(u32::MAX)))
}

/// Straight-line driver-body builder over the public MIR op set.
pub(crate) struct Body {
    pub(crate) stmts: Vec<Stmt>,
    next: u32,
}

impl Body {
    pub(crate) fn new() -> Self {
        Self {
            stmts: Vec::new(),
            next: 0,
        }
    }

    pub(crate) fn var(&mut self) -> Var {
        let var = Var(self.next);
        self.next += 1;
        var
    }

    pub(crate) fn record(&mut self, tag: AtomRef, args: Vec<Value>) -> Var {
        let dst = self.var();
        self.stmts.push(Stmt::RecordNew {
            dst,
            tag,
            args,
            span: sp(),
        });
        dst
    }

    pub(crate) fn list(&mut self, items: Vec<Value>) -> Var {
        let dst = self.var();
        self.stmts.push(Stmt::ListNew {
            dst,
            items,
            span: sp(),
        });
        dst
    }

    pub(crate) fn call_local(&mut self, callee: FnRef, args: Vec<Value>) -> Var {
        let dst = self.var();
        self.stmts.push(Stmt::CallLocal {
            dst: Some(dst),
            callee,
            args,
            live_after: aion_awl::mir::LiveAfter::default(),
            span: sp(),
        });
        dst
    }

    pub(crate) fn closure(&mut self, lifted: FnRef) -> Var {
        let dst = self.var();
        self.stmts.push(Stmt::MakeClosure {
            dst,
            lifted,
            captures: Vec::new(),
            span: sp(),
        });
        dst
    }

    pub(crate) fn call_rt(&mut self, callee: RuntimeFn, args: Vec<Value>) -> Var {
        let dst = self.var();
        self.stmts.push(Stmt::CallRt {
            dst: Some(dst),
            callee,
            args,
            live_after: aion_awl::mir::LiveAfter::default(),
            span: sp(),
        });
        dst
    }

    pub(crate) fn field(&mut self, base: Var, index: u16) -> Var {
        let dst = self.var();
        self.stmts.push(Stmt::FieldGet {
            dst,
            base: Value::Var(base),
            index,
            span: sp(),
        });
        dst
    }

    pub(crate) fn call_fun(&mut self, fun: Var, args: Vec<Value>) -> Var {
        let dst = self.var();
        self.stmts.push(Stmt::CallClosure {
            dst: Some(dst),
            fun: Value::Var(fun),
            args,
            live_after: aion_awl::mir::LiveAfter::default(),
            span: sp(),
        });
        dst
    }

    pub(crate) fn cmp_eq(&mut self, lhs: Value, rhs: Value) -> Var {
        let dst = self.var();
        self.stmts.push(Stmt::Cmp {
            dst,
            op: aion_awl::mir::CmpOp::Eq,
            lhs,
            rhs,
            span: sp(),
        });
        dst
    }

    pub(crate) fn tuple(&mut self, items: Vec<Value>) -> Var {
        let dst = self.var();
        self.stmts.push(Stmt::TupleNew {
            dst,
            items,
            span: sp(),
        });
        dst
    }

    /// `codec` composer call → `(encode_fun, decode_fun)` closure vars
    /// (`Codec(encode, decode)`: elements 1 and 2 past the tag).
    pub(crate) fn codec_funs(&mut self, codec: FnRef) -> (Var, Var) {
        let value = self.call_local(codec, Vec::new());
        let encode = self.field(value, 1);
        let decode = self.field(value, 2);
        (encode, decode)
    }

    /// Round-trip driver tail: `#(encoded, decode(encoded) == Ok(value))`.
    /// Takes a `Value` so whole-value atoms (`None`) round-trip too.
    pub(crate) fn roundtrip_tail(&mut self, codec: FnRef, value: Value, ok_atom: AtomRef) -> Block {
        let (encode, decode) = self.codec_funs(codec);
        let encoded = self.call_fun(encode, vec![value.clone()]);
        let decoded = self.call_fun(decode, vec![Value::Var(encoded)]);
        let expected = self.record(ok_atom, vec![value]);
        let equal = self.cmp_eq(Value::Var(decoded), Value::Var(expected));
        let out = self.tuple(vec![Value::Var(encoded), Value::Var(equal)]);
        Block {
            stmts: std::mem::take(&mut self.stmts),
            tail: Tail::Return(Value::Var(out)),
        }
    }
}

/// Append one exported arity-0 driver function to a lowered module.
pub(crate) fn push_driver(module: &mut MirModule, name: &str, body: Block) {
    let reference = FnRef(u32::try_from(module.functions.len()).unwrap_or(u32::MAX));
    module.functions.push(MirFn::Flow(FlowFn {
        origin: FnOrigin::Region {
            entry_step: name.to_owned(),
        },
        name: name.to_owned(),
        params: Vec::new(),
        param_tys: Vec::new(),
        ret_ty: TyDesc::Dynamic,
        body,
        span: sp(),
        degraded_parallel: false,
    }));
    module.exports.push(reference);
}

pub(crate) fn lowered(relative: &str) -> Result<MirModule, Box<dyn Error>> {
    let path = fixture(relative);
    let source = fs::read_to_string(&path)?;
    let document = parse(&source)?;
    Ok(lower(&document, path.parent())?)
}

/// Append the direct-side drivers to the lowered `optional_shorthand` module.
pub(crate) fn note_drivers(module: &mut MirModule) -> Result<(), Box<dyn Error>> {
    let codec = fn_by_name(module, "note_codec")?;
    let note = atom_ref(module, "note");
    let some = atom_ref(module, "some");
    let none_atom = atom_ref(module, "none");
    let ok = atom_ref(module, "ok");
    let error = atom_ref(module, "error");
    let true_atom = atom_ref(module, "true");
    let false_atom = atom_ref(module, "false");

    // #(encode(Note("t", Some("b"), ["x","y"])), roundtrip)
    let t = lit_ref(module, "t");
    let b = lit_ref(module, "b");
    let x = lit_ref(module, "x");
    let y = lit_ref(module, "y");
    let mut body = Body::new();
    let some_b = body.record(some, vec![Value::Lit(b)]);
    let tags = body.list(vec![Value::Lit(x), Value::Lit(y)]);
    let value = body.record(
        note,
        vec![Value::Lit(t), Value::Var(some_b), Value::Var(tags)],
    );
    let block = body.roundtrip_tail(codec, Value::Var(value), ok);
    push_driver(module, "awl$rt_note_some", block);

    // #(encode(Note("t", None, [])), roundtrip)
    let mut body = Body::new();
    let value = body.record(
        note,
        vec![Value::Lit(t), Value::Atom(none_atom), Value::Nil],
    );
    let block = body.roundtrip_tail(codec, Value::Var(value), ok);
    push_driver(module, "awl$rt_note_none", block);

    // Whole-value nullable composite trio (panel minor — the untested half
    // of the S3 optional split): `Some("b")` ⇄ `"b"`, `None` ⇄ `null`
    // through `option_string_codec` (json.nullable / decode.optional).
    let option_codec = fn_by_name(module, "option_string_codec")?;
    let mut body = Body::new();
    let some_b = body.record(some, vec![Value::Lit(b)]);
    let block = body.roundtrip_tail(option_codec, Value::Var(some_b), ok);
    push_driver(module, "awl$rt_option_some", block);

    let mut body = Body::new();
    let block = body.roundtrip_tail(option_codec, Value::Atom(none_atom), ok);
    push_driver(module, "awl$rt_option_none", block);

    // Explicit null for the optional field must FAIL (D4).
    let null_json = lit_ref(module, r#"{"title":"t","body":null,"tags":[]}"#);
    push_driver(
        module,
        "awl$rt_note_null_fails",
        decode_is_error(codec, null_json, error, true_atom, false_atom),
    );

    // Absence decodes to None (expected value built BEFORE the decode call:
    // beamr 0.14's TestHeap does not grow a nearly-full heap late in this
    // driver, and allocation-first matches generated-code shape anyway).
    let absent_json = lit_ref(module, r#"{"title":"t","tags":[]}"#);
    let mut body = Body::new();
    let expected_value = body.record(
        note,
        vec![Value::Lit(t), Value::Atom(none_atom), Value::Nil],
    );
    let expected = body.record(ok, vec![Value::Var(expected_value)]);
    let (_, decode) = body.codec_funs(codec);
    let decoded = body.call_fun(decode, vec![Value::Lit(absent_json)]);
    let equal = body.cmp_eq(Value::Var(decoded), Value::Var(expected));
    push_driver(
        module,
        "awl$rt_note_absent_is_none",
        Block {
            stmts: std::mem::take(&mut body.stmts),
            tail: Tail::Return(Value::Var(equal)),
        },
    );
    Ok(())
}

/// Append the direct-side drivers to the lowered `triage_message` module.
pub(crate) fn triage_drivers(module: &mut MirModule) -> Result<(), Box<dyn Error>> {
    let verdict_codec = fn_by_name(module, "verdict_codec")?;
    let category_codec = fn_by_name(module, "category_codec")?;
    let union_codec = fn_by_name(module, "triage_message_outcome_codec")?;
    let verdict = atom_ref(module, "verdict");
    let urgent = atom_ref(module, "urgent");
    let triaged = atom_ref(module, "triaged_outcome");
    let ok = atom_ref(module, "ok");
    let error = atom_ref(module, "error");
    let true_atom = atom_ref(module, "true");
    let false_atom = atom_ref(module, "false");
    let check = lit_ref(module, "check");

    let mut body = Body::new();
    let value = body.record(verdict, vec![Value::Atom(urgent), Value::Lit(check)]);
    let block = body.roundtrip_tail(verdict_codec, Value::Var(value), ok);
    push_driver(module, "awl$rt_verdict", block);

    let mut body = Body::new();
    let payload = body.record(verdict, vec![Value::Atom(urgent), Value::Lit(check)]);
    let value = body.record(triaged, vec![Value::Var(payload)]);
    let block = body.roundtrip_tail(union_codec, Value::Var(value), ok);
    push_driver(module, "awl$rt_union", block);

    let bogus_category = lit_ref(module, "\"Bogus\"");
    push_driver(
        module,
        "awl$rt_category_unknown_fails",
        decode_is_error(category_codec, bogus_category, error, true_atom, false_atom),
    );

    let bogus_union = lit_ref(module, r#"{"outcome":"bogus","payload":{}}"#);
    push_driver(
        module,
        "awl$rt_union_unknown_fails",
        decode_is_error(union_codec, bogus_union, error, true_atom, false_atom),
    );
    Ok(())
}

fn finding_list(
    body: &mut Body,
    finding: AtomRef,
    blocker: LitRef,
    warning: LitRef,
    true_atom: AtomRef,
    false_atom: AtomRef,
) -> Var {
    let blocking = body.record(finding, vec![Value::Lit(blocker), Value::Atom(true_atom)]);
    let non_blocking = body.record(finding, vec![Value::Lit(warning), Value::Atom(false_atom)]);
    body.list(vec![Value::Var(blocking), Value::Var(non_blocking)])
}

fn list_predicate_tail(mut body: Body, list: Var, predicate: FnRef, runtime: RuntimeFn) -> Block {
    let fun = body.closure(predicate);
    let result = body.call_rt(runtime, vec![Value::Var(list), Value::Var(fun)]);
    Block {
        stmts: body.stmts,
        tail: Tail::Return(Value::Var(result)),
    }
}

/// Execute the lowered fixture's own lifted predicates through the same
/// `gleam/list` runtime calls selected for production workflow code.
pub(crate) fn collection_predicate_drivers(module: &mut MirModule) -> Result<(), Box<dyn Error>> {
    let any_finding = fn_by_name(module, "awl_predicate_0")?;
    let all_finding = fn_by_name(module, "awl_predicate_1")?;
    let any_round = fn_by_name(module, "awl_predicate_2")?;
    let finding = atom_ref(module, "finding");
    let round = atom_ref(module, "round");
    let true_atom = atom_ref(module, "true");
    let false_atom = atom_ref(module, "false");
    let blocker = lit_ref(module, "blocker");
    let warning = lit_ref(module, "warning");

    // Keep one runtime call per driver. This mirrors generated region bodies
    // and avoids making test-only assertions about cross-call liveness.
    let mut body = Body::new();
    let findings = finding_list(&mut body, finding, blocker, warning, true_atom, false_atom);
    let block = list_predicate_tail(body, findings, any_finding, RuntimeFn::LAny);
    push_driver(module, "awl$rt_any", block);

    let mut body = Body::new();
    let findings = finding_list(&mut body, finding, blocker, warning, true_atom, false_atom);
    let block = list_predicate_tail(body, findings, all_finding, RuntimeFn::LAll);
    push_driver(module, "awl$rt_all", block);

    let mut body = Body::new();
    let round_findings = finding_list(&mut body, finding, blocker, warning, true_atom, false_atom);
    let round_value = body.record(round, vec![Value::Var(round_findings)]);
    let rounds = body.list(vec![Value::Var(round_value)]);
    let block = list_predicate_tail(body, rounds, any_round, RuntimeFn::LAny);
    push_driver(module, "awl$rt_nested_any", block);

    let mut body = Body::new();
    let empty = body.list(Vec::new());
    let block = list_predicate_tail(body, empty, all_finding, RuntimeFn::LAll);
    push_driver(module, "awl$rt_empty_all", block);

    let mut body = Body::new();
    let empty = body.list(Vec::new());
    let block = list_predicate_tail(body, empty, any_finding, RuntimeFn::LAny);
    push_driver(module, "awl$rt_empty_any", block);
    Ok(())
}

/// Export a zero-arity test shim that constructs only the input value, then
/// calls the production-generated `execute/1` host unchanged.
pub(crate) fn production_execute_driver(module: &mut MirModule, owners: &[&str]) {
    let input_name = format!("{}_input", module.name);
    let input = atom_ref(module, &input_name);
    let finding = atom_ref(module, "finding");
    let owner_literals = owners
        .iter()
        .map(|owner| lit_ref(module, owner))
        .collect::<Vec<_>>();
    let mut body = Body::new();
    let mut values = Vec::new();
    for owner in owner_literals {
        let item = body.record(finding, vec![Value::Lit(owner)]);
        values.push(Value::Var(item));
    }
    let findings = body.list(values);
    let input_value = body.record(input, vec![Value::Var(findings)]);
    let result = body.call_local(FnRef(2), vec![Value::Var(input_value)]);
    push_driver(
        module,
        "awl$rt_execute",
        Block {
            stmts: body.stmts,
            tail: Tail::Return(Value::Var(result)),
        },
    );
}

/// `case codec.decode(json) { Error(_) -> true _ -> false }` as a driver body.
pub(crate) fn decode_is_error(
    codec: FnRef,
    json: LitRef,
    error: AtomRef,
    true_atom: AtomRef,
    false_atom: AtomRef,
) -> Block {
    let mut body = Body::new();
    let (_, decode) = body.codec_funs(codec);
    let decoded = body.call_fun(decode, vec![Value::Lit(json)]);
    Block {
        stmts: body.stmts,
        tail: Tail::If {
            test: Test::IsTagged {
                value: Value::Var(decoded),
                tag: error,
                arity: 2,
            },
            then_block: Box::new(Block {
                stmts: Vec::new(),
                tail: Tail::Return(Value::Atom(true_atom)),
            }),
            else_block: Box::new(Block {
                stmts: Vec::new(),
                tail: Tail::Return(Value::Atom(false_atom)),
            }),
            span: sp(),
        },
    }
}
