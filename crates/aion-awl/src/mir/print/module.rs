//! The `print_mir` entry point and the module-level golden sections: exports,
//! the effect schedule (S11), the capability summary (S16), the atom table,
//! and the type registry.

use std::fmt::Write as _;

use super::super::func::MirFn;
use super::super::ops::{Block, Stmt, Tail};
use super::super::runtime::{DurableFamily, RuntimeFn};
use super::super::shapes::TypeShape;
use super::super::unit::MirModule;
use super::function::print_function;
use super::util::{family_name, render_values, render_wire};

/// Render a `MirModule` to its canonical golden text.
#[must_use]
pub fn print_mir(module: &MirModule) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "module {} source={}", module.name, module.source);
    print_exports(module, &mut out);
    print_effect_schedule(module, &mut out);
    print_capability_summary(module, &mut out);
    print_atoms(module, &mut out);
    print_literals(module, &mut out);
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

fn print_atoms(module: &MirModule, out: &mut String) {
    out.push_str("== atoms ==\n");
    for (index, atom) in module.atoms.iter().enumerate() {
        let _ = writeln!(out, "  [{index}] {atom}");
    }
}

/// The literal table (R5 two-sided codec-identity pins): every `lit#N`
/// operand's CONTENT is golden-visible, so swapping two same-shaped literals
/// (an action-name pair, a field name) can never pass a pin.
fn print_literals(module: &MirModule, out: &mut String) {
    out.push_str("== literals ==\n");
    for (index, literal) in module.literals.iter().enumerate() {
        let _ = writeln!(
            out,
            "  [{index}] {}",
            super::util::render_literal(module, literal)
        );
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
