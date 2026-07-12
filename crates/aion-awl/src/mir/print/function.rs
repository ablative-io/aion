//! Per-function golden rendering: signatures, `CodecTemplate` provenance
//! headers (S8), template-shell lines, and the statement/tail body tree.

use std::fmt::Write as _;

use super::super::func::{CodecRef, FnOrigin, MirFn, TemplateFn, TrioParams};
use super::super::ops::{Block, JsonVal, Stmt, Tail, Test, ToJsonRef};
use super::super::unit::MirModule;
use super::util::{
    fn_name, opt_var, render_live, render_tydesc, render_value, render_values, render_wire, var,
};

pub(super) fn print_function(module: &MirModule, function: &MirFn, out: &mut String) {
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
