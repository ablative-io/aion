//! Canonical MIR golden printer (AWL-BC-IR.md §9).
//!
//! First section = the effect schedule (S11: durable `CallRt`s in order with
//! wire-visible arguments), then the capability summary (S16), the type
//! registry, and per-function bodies with `live_after` (S14),
//! `degraded_parallel` (S13), and `CodecTemplate` provenance headers (S8).
//! Deterministic: no timestamps, no paths beyond the source file name.

use std::fmt::Write as _;

use super::func::{CodecRef, FnOrigin, MirFn, TemplateFn, TrioParams};
use super::ids::{FnRef, Var};
use super::ops::{Block, JsonVal, LiveAfter, Stmt, Tail, Test, ToJsonRef, Value};
use super::runtime::{DurableFamily, RuntimeFn};
use super::shapes::{TypeShape, WireDesc};
use super::tydesc::TyDesc;
use super::unit::MirModule;

/// Render a `MirModule` to its canonical golden text.
#[must_use]
pub fn print_mir(module: &MirModule) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "module {} source={}", module.name, module.source);
    print_exports(module, &mut out);
    print_effect_schedule(module, &mut out);
    print_capability_summary(module, &mut out);
    print_atoms(module, &mut out);
    print_types(module, &mut out);
    for function in &module.functions {
        print_function(module, function, &mut out);
    }
    out
}

fn print_exports(module: &MirModule, out: &mut String) {
    let names: Vec<String> = module
        .exports
        .iter()
        .filter_map(|reference| module.function(*reference))
        .map(|function| format!("{}/{}", function.name(), MirModule::arity(function)))
        .collect();
    let _ = writeln!(out, "exports: {}", names.join(", "));
}

fn print_effect_schedule(module: &MirModule, out: &mut String) {
    out.push_str("== effect schedule ==\n");
    for function in &module.functions {
        let MirFn::Flow(flow) = function else {
            continue;
        };
        let mut calls = Vec::new();
        collect_durable(module, &flow.body, &mut calls);
        if calls.is_empty() {
            continue;
        }
        let _ = writeln!(out, "  {}:", flow.name);
        for line in calls {
            let _ = writeln!(out, "    {line}");
        }
    }
}

fn collect_durable(module: &MirModule, block: &Block, out: &mut Vec<String>) {
    for stmt in &block.stmts {
        if let Some(callee) = stmt.runtime_callee()
            && let Some(family) = callee.durable_family()
        {
            let args = match stmt {
                Stmt::CallRt { args, .. } => render_values(module, args),
                _ => String::new(),
            };
            out.push(format!(
                "{} [{}] ({args})",
                callee.label(),
                family_name(family)
            ));
        }
        if let Stmt::Attempt { on_ok, on_err, .. } = stmt {
            collect_durable(module, on_ok, out);
            collect_durable(module, on_err, out);
        }
    }
    match &block.tail {
        Tail::If {
            then_block,
            else_block,
            ..
        } => {
            collect_durable(module, then_block, out);
            collect_durable(module, else_block, out);
        }
        Tail::SelectEnum { arms, .. } => {
            for (_, arm) in arms {
                collect_durable(module, arm, out);
            }
        }
        Tail::Return(_) | Tail::TailLocal { .. } | Tail::TailRt { .. } => {}
    }
}

fn print_capability_summary(module: &MirModule, out: &mut String) {
    let mut used: Vec<DurableFamily> = Vec::new();
    for function in &module.functions {
        if let MirFn::Flow(flow) = function {
            gather_families(&flow.body, &mut used);
        }
    }
    used.sort_unstable();
    used.dedup();
    let names: Vec<&str> = used.into_iter().map(family_name).collect();
    let _ = writeln!(out, "== durable families == {}", names.join(", "));
}

fn gather_families(block: &Block, out: &mut Vec<DurableFamily>) {
    for stmt in &block.stmts {
        if let Some(family) = stmt.runtime_callee().and_then(RuntimeFn::durable_family) {
            out.push(family);
        }
        if let Stmt::Attempt { on_ok, on_err, .. } = stmt {
            gather_families(on_ok, out);
            gather_families(on_err, out);
        }
    }
    match &block.tail {
        Tail::If {
            then_block,
            else_block,
            ..
        } => {
            gather_families(then_block, out);
            gather_families(else_block, out);
        }
        Tail::SelectEnum { arms, .. } => {
            for (_, arm) in arms {
                gather_families(arm, out);
            }
        }
        Tail::Return(_) | Tail::TailLocal { .. } | Tail::TailRt { .. } => {}
    }
}

const fn family_name(family: DurableFamily) -> &'static str {
    match family {
        DurableFamily::Timers => "timers",
        DurableFamily::Activities => "activities",
        DurableFamily::Children => "children",
        DurableFamily::Signals => "signals",
    }
}

fn print_atoms(module: &MirModule, out: &mut String) {
    out.push_str("== atoms ==\n");
    for (index, atom) in module.atoms.iter().enumerate() {
        let _ = writeln!(out, "  [{index}] {atom}");
    }
}

fn print_types(module: &MirModule, out: &mut String) {
    out.push_str("== types ==\n");
    for shape in &module.types {
        match shape {
            TypeShape::Record { name, tag, fields } => {
                let tag_name = module.atom(tag.0).unwrap_or("?");
                let _ = writeln!(out, "  record {name} tag={tag_name}");
                for field in fields {
                    let opt = if field.optional { " optional" } else { "" };
                    let _ = writeln!(
                        out,
                        "    {}: {}{opt}",
                        field.awl_name,
                        render_wire(&field.desc)
                    );
                }
            }
            TypeShape::Enum { name, variants } => {
                let _ = writeln!(out, "  enum {name}");
                for (ctor, json) in variants {
                    let ctor_name = module.atom(ctor.0).unwrap_or("?");
                    let _ = writeln!(out, "    {ctor_name} = \"{json}\"");
                }
            }
            TypeShape::Union { name, arms } => {
                let _ = writeln!(out, "  union {name}");
                for arm in arms {
                    let ctor_name = module.atom(arm.ctor.0).unwrap_or("?");
                    let _ = writeln!(
                        out,
                        "    {} -> {ctor_name}({})",
                        arm.outcome,
                        render_wire(&arm.payload)
                    );
                }
            }
        }
    }
}

fn render_wire(desc: &WireDesc) -> String {
    match desc {
        WireDesc::Bool => "bool".to_owned(),
        WireDesc::Int => "int".to_owned(),
        WireDesc::Float => "float".to_owned(),
        WireDesc::Str => "string".to_owned(),
        WireDesc::Nil => "nil".to_owned(),
        WireDesc::List(inner) => format!("list({})", render_wire(inner)),
        WireDesc::Nullable(inner) => format!("nullable({})", render_wire(inner)),
        WireDesc::Ref(name) => format!("ref({name})"),
    }
}

fn print_function(module: &MirModule, function: &MirFn, out: &mut String) {
    let arity = MirModule::arity(function);
    let _ = writeln!(
        out,
        "== fn {}/{arity} origin={} ==",
        function.name(),
        render_origin(module, function.origin())
    );
    let sig = function
        .param_tys()
        .iter()
        .map(render_tydesc)
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        out,
        "  sig: ({sig}) -> {}",
        render_tydesc(function.ret_ty())
    );
    match function {
        MirFn::Templated { template, .. } => print_template(module, template, out),
        MirFn::Flow(flow) => {
            if flow.degraded_parallel {
                out.push_str("  degraded_parallel: true\n");
            }
            print_block(module, &flow.body, 1, out);
        }
    }
}

fn render_origin(module: &MirModule, origin: &FnOrigin) -> String {
    match origin {
        FnOrigin::Run => "run".to_owned(),
        FnOrigin::Definition => "definition".to_owned(),
        FnOrigin::Execute => "execute".to_owned(),
        FnOrigin::Region { entry_step } => format!("region({entry_step})"),
        FnOrigin::SubStep { parent, sub } => format!("substep({parent}.{sub})"),
        FnOrigin::Loop { step, index } => format!("loop({step}#{index})"),
        FnOrigin::ActivityWrapper { action, raw } => {
            format!("activity_wrapper({action}, raw={raw})")
        }
        FnOrigin::SignalRef { signal } => format!("signal_ref({signal})"),
        FnOrigin::DeadBody => "dead_body".to_owned(),
        FnOrigin::ChildWitness => "child_witness".to_owned(),
        FnOrigin::CodecTemplate {
            kind,
            subject,
            params,
        } => {
            format!(
                "codec_template({kind:?}, {subject}, {})",
                render_trio(params)
            )
        }
        FnOrigin::LiftedClosure { host, index } => {
            format!("lifted({}, #{index})", fn_name(module, *host))
        }
    }
}

fn render_trio(params: &TrioParams) -> String {
    match params {
        TrioParams::Record { shape } => format!("record#{}", shape.0),
        TrioParams::Enum { shape } => format!("enum#{}", shape.0),
        TrioParams::Union { shape } => format!("union#{}", shape.0),
        TrioParams::Composite { desc } => format!("composite({})", render_wire(desc)),
    }
}

fn print_template(module: &MirModule, template: &TemplateFn, out: &mut String) {
    let line = match template {
        TemplateFn::Definition {
            workflow_name,
            input_codec,
            output_codec,
        } => format!(
            "T-DEF name=\"{workflow_name}\" input={} output={}",
            fn_name(module, *input_codec),
            render_codec(module, output_codec)
        ),
        TemplateFn::Run {
            input_codec,
            output_codec,
        } => format!(
            "T-RUN input={} output={}",
            fn_name(module, *input_codec),
            render_codec(module, output_codec)
        ),
        TemplateFn::Execute {
            input_fields,
            entry,
            entry_args,
        } => format!(
            "T-EXEC fields=[{}] entry={} args={entry_args:?}",
            input_fields
                .iter()
                .map(|(name, ty)| format!("{name}:{}", render_tydesc(ty)))
                .collect::<Vec<_>>()
                .join(", "),
            fn_name(module, *entry)
        ),
        TemplateFn::ActivityWrapper {
            action,
            input_codec,
            return_codec,
            ..
        } => format!(
            "T-ACT action={action} input_codec={} return={}",
            fn_name(module, *input_codec),
            render_codec(module, return_codec)
        ),
        TemplateFn::ActivityWrapperRaw {
            action,
            input_codec,
            ..
        } => {
            format!(
                "T-ACTRAW action={action} input_codec={}",
                fn_name(module, *input_codec)
            )
        }
        TemplateFn::SignalRef {
            signal,
            payload_codec,
        } => {
            format!(
                "T-SIG signal={signal} payload={}",
                render_codec(module, payload_codec)
            )
        }
        TemplateFn::DeadBody => "T-DEAD".to_owned(),
        TemplateFn::ChildWitness => "T-WIT".to_owned(),
    };
    let _ = writeln!(out, "  {line}");
}

fn render_codec(module: &MirModule, codec: &CodecRef) -> String {
    match codec {
        CodecRef::Local(reference) => fn_name(module, *reference),
        CodecRef::SdkNil => "awlc.nil_codec".to_owned(),
        CodecRef::SdkLeaf(leaf) => format!("awlc.{}_codec", leaf.stem()),
    }
}

fn print_block(module: &MirModule, block: &Block, indent: usize, out: &mut String) {
    for stmt in &block.stmts {
        let pad = "  ".repeat(indent);
        let _ = writeln!(out, "{pad}{}", render_stmt(module, stmt));
        if let Stmt::Attempt { on_ok, on_err, .. } = stmt {
            let _ = writeln!(out, "{pad}  on_ok:");
            print_block(module, on_ok, indent + 2, out);
            let _ = writeln!(out, "{pad}  on_err:");
            print_block(module, on_err, indent + 2, out);
        }
    }
    let pad = "  ".repeat(indent);
    match &block.tail {
        Tail::Return(value) => {
            let _ = writeln!(out, "{pad}return {}", render_value(module, value));
        }
        Tail::TailLocal { callee, args } => {
            let _ = writeln!(
                out,
                "{pad}tail_local {}({})",
                fn_name(module, *callee),
                render_values(module, args)
            );
        }
        Tail::TailRt { callee, args } => {
            let _ = writeln!(
                out,
                "{pad}tail_rt {}({})",
                callee.label(),
                render_values(module, args)
            );
        }
        Tail::If {
            test,
            then_block,
            else_block,
            ..
        } => {
            let _ = writeln!(out, "{pad}if {}:", render_test(module, test));
            print_block(module, then_block, indent + 1, out);
            let _ = writeln!(out, "{pad}else:");
            print_block(module, else_block, indent + 1, out);
        }
        Tail::SelectEnum { subject, arms, .. } => {
            let _ = writeln!(out, "{pad}select {} {{", render_value(module, subject));
            for (atom, arm) in arms {
                let name = module.atom(atom.0).unwrap_or("?");
                let _ = writeln!(out, "{pad}  {name} ->");
                print_block(module, arm, indent + 2, out);
            }
            let _ = writeln!(out, "{pad}}}");
        }
    }
}

fn render_stmt(module: &MirModule, stmt: &Stmt) -> String {
    match stmt {
        Stmt::Bind { dst, value, .. } => {
            format!("{} = {}", var(*dst), render_value(module, value))
        }
        Stmt::FieldGet {
            dst, base, index, ..
        } => {
            format!(
                "{} = field({}, {index})",
                var(*dst),
                render_value(module, base)
            )
        }
        Stmt::RecordNew { dst, tag, args, .. } => format!(
            "{} = record({}, [{}])",
            var(*dst),
            module.atom(tag.0).unwrap_or("?"),
            render_values(module, args)
        ),
        Stmt::ListNew { dst, items, .. } => {
            format!("{} = list([{}])", var(*dst), render_values(module, items))
        }
        Stmt::CallRt {
            dst,
            callee,
            args,
            live_after,
            ..
        } => format!(
            "{} = call_rt {}({}){}",
            opt_var(*dst),
            callee.label(),
            render_values(module, args),
            render_live(live_after)
        ),
        Stmt::CallLocal {
            dst,
            callee,
            args,
            live_after,
            ..
        } => format!(
            "{} = call_local {}({}){}",
            opt_var(*dst),
            fn_name(module, *callee),
            render_values(module, args),
            render_live(live_after)
        ),
        Stmt::CallClosure {
            dst,
            fun,
            args,
            live_after,
            ..
        } => format!(
            "{} = call_closure {}({}){}",
            opt_var(*dst),
            render_value(module, fun),
            render_values(module, args),
            render_live(live_after)
        ),
        Stmt::MakeClosure {
            dst,
            lifted,
            captures,
            ..
        } => format!(
            "{} = make_closure {} captures=[{}]",
            var(*dst),
            fn_name(module, *lifted),
            render_values(module, captures)
        ),
        Stmt::TryBind {
            dst,
            result,
            live_after,
            ..
        } => {
            format!(
                "{} = try_bind {}{}",
                var(*dst),
                var(*result),
                render_live(live_after)
            )
        }
        Stmt::WaitTimeoutCase {
            dst,
            receive,
            deadline_ms,
            ..
        } => format!(
            "{} = wait_timeout {} deadline={deadline_ms}",
            var(*dst),
            fn_name(module, *receive)
        ),
        _ => render_stmt_ops(module, stmt),
    }
}

fn render_stmt_ops(module: &MirModule, stmt: &Stmt) -> String {
    match stmt {
        Stmt::Cmp {
            dst, op, lhs, rhs, ..
        } => format!(
            "{} = cmp {op:?} {} {}",
            var(*dst),
            render_value(module, lhs),
            render_value(module, rhs)
        ),
        Stmt::BoolOp {
            dst, op, lhs, rhs, ..
        } => format!(
            "{} = boolop {op:?} {} {}",
            var(*dst),
            render_value(module, lhs),
            render_value(module, rhs)
        ),
        Stmt::Not { dst, src, .. } => format!("{} = not {}", var(*dst), render_value(module, src)),
        Stmt::Concat { dst, lhs, rhs, .. } => format!(
            "{} = concat {} {}",
            var(*dst),
            render_value(module, lhs),
            render_value(module, rhs)
        ),
        Stmt::Increment { dst, src, .. } => format!("{} = increment {}", var(*dst), var(*src)),
        Stmt::AssertList { binds, list, .. } => {
            let names: Vec<String> = binds
                .iter()
                .map(|bind| bind.map_or_else(|| "_".to_owned(), var))
                .collect();
            format!("assert_list [{}] = {}", names.join(", "), var(*list))
        }
        Stmt::AssertSome { dst, option, .. } => {
            format!("{} = assert_some {}", var(*dst), var(*option))
        }
        Stmt::JsonObj { dst, pairs, .. } => {
            let rendered: Vec<String> = pairs
                .iter()
                .map(|(name, value)| format!("\"{name}\": {}", render_json(module, value)))
                .collect();
            format!("{} = json_obj {{{}}}", var(*dst), rendered.join(", "))
        }
        Stmt::IndexGuard {
            dst, base, index, ..
        } => {
            format!("{} = index_guard {}[{index}]", var(*dst), var(*base))
        }
        Stmt::Attempt {
            lifted,
            captures,
            defs,
            ..
        } => format!(
            "attempt {} captures=[{}] defs=[{}]",
            fn_name(module, *lifted),
            render_values(module, captures),
            defs.iter().map(|d| var(*d)).collect::<Vec<_>>().join(", ")
        ),
        _ => String::new(),
    }
}

fn render_json(module: &MirModule, value: &JsonVal) -> String {
    match value {
        JsonVal::Encoded { value, via } => {
            let via = match via {
                ToJsonRef::SdkLeaf(leaf) => format!("awlc.{}_to_json", leaf.stem()),
                ToJsonRef::Local(reference) => fn_name(module, *reference),
            };
            format!("{}|>{via}", render_value(module, value))
        }
    }
}

fn render_test(module: &MirModule, test: &Test) -> String {
    match test {
        Test::IsTrue(value) => format!("is_true {}", render_value(module, value)),
        Test::Cmp { op, lhs, rhs } => format!(
            "cmp {op:?} {} {}",
            render_value(module, lhs),
            render_value(module, rhs)
        ),
        Test::IsTagged { value, tag, arity } => format!(
            "is_tagged {} {}/{arity}",
            render_value(module, value),
            module.atom(tag.0).unwrap_or("?")
        ),
        Test::Not(inner) => format!("not({})", render_test(module, inner)),
    }
}

fn render_values(module: &MirModule, values: &[Value]) -> String {
    values
        .iter()
        .map(|value| render_value(module, value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_value(module: &MirModule, value: &Value) -> String {
    match value {
        Value::Var(v) => var(*v),
        Value::Lit(reference) => format!("lit#{}", reference.0),
        Value::Atom(atom) => format!("'{}'", module.atom(atom.0).unwrap_or("?")),
        Value::Int(value) => value.to_string(),
        Value::Nil => "nil".to_owned(),
    }
}

fn render_live(live: &LiveAfter) -> String {
    if live.0.is_empty() {
        String::new()
    } else {
        format!(
            " live_after=[{}]",
            live.0
                .iter()
                .map(|v| var(*v))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn var(v: Var) -> String {
    format!("v{}", v.0)
}

fn opt_var(v: Option<Var>) -> String {
    v.map_or_else(|| "_".to_owned(), var)
}

fn fn_name(module: &MirModule, reference: FnRef) -> String {
    module.function(reference).map_or_else(
        || format!("fn#{}", reference.0),
        |function| function.name().to_owned(),
    )
}

fn render_tydesc(ty: &TyDesc) -> String {
    match ty {
        TyDesc::Bool => "Bool".to_owned(),
        TyDesc::Int => "Int".to_owned(),
        TyDesc::Float => "Float".to_owned(),
        TyDesc::String => "String".to_owned(),
        TyDesc::Nil => "Nil".to_owned(),
        TyDesc::List(inner) => format!("List({})", render_tydesc(inner)),
        TyDesc::Option(inner) => format!("Option({})", render_tydesc(inner)),
        TyDesc::Result(ok, err) => format!("Result({}, {})", render_tydesc(ok), render_tydesc(err)),
        TyDesc::Tuple(elements) => format!(
            "#({})",
            elements
                .iter()
                .map(render_tydesc)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TyDesc::Custom {
            module,
            name,
            params,
        } => {
            if params.is_empty() {
                format!("{module}.{name}")
            } else {
                format!(
                    "{module}.{name}({})",
                    params
                        .iter()
                        .map(render_tydesc)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        TyDesc::Fn(args, ret) => format!(
            "fn({}) -> {}",
            args.iter()
                .map(render_tydesc)
                .collect::<Vec<_>>()
                .join(", "),
            render_tydesc(ret)
        ),
        TyDesc::Dynamic => "Dynamic".to_owned(),
        TyDesc::Json => "Json".to_owned(),
        TyDesc::AwlError => "AwlError".to_owned(),
        TyDesc::Decoder(inner) => format!("Decoder({})", render_tydesc(inner)),
        TyDesc::Codec(inner) => format!("Codec({})", render_tydesc(inner)),
        TyDesc::Activity(input, output) => {
            format!(
                "Activity({}, {})",
                render_tydesc(input),
                render_tydesc(output)
            )
        }
        TyDesc::SignalRef(inner) => format!("Signal({})", render_tydesc(inner)),
        TyDesc::WorkflowDefinition(input, output, error) => format!(
            "WorkflowDefinition({}, {}, {})",
            render_tydesc(input),
            render_tydesc(output),
            render_tydesc(error)
        ),
        TyDesc::Duration => "Duration".to_owned(),
        TyDesc::Unknown => "Unknown".to_owned(),
    }
}
