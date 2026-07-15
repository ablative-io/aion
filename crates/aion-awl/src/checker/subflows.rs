//! Subflow shape rules: a subflow is used as a step (its invocation is the
//! only statement of a top-level step; never a pipe stage or a nested
//! statement), and subflows compile inline, so invocation cycles —
//! direct or mutual — are unwritable.

use std::collections::BTreeMap;

use crate::ast::{PipeStage, Statement, Step, SubflowDecl};

use super::context::Ctx;

/// Run the subflow shape pass: placement of every invocation, then the
/// inline-compilation recursion rule.
pub(super) fn run(ctx: &mut Ctx<'_>) {
    let doc = ctx.doc;
    check_placement(ctx, &doc.steps);
    for subflow in &doc.subflows {
        check_placement(ctx, &subflow.steps);
    }
    check_recursion(ctx, &doc.subflows);
}

/// A subflow invocation is its own step: the single statement of a
/// top-level step body. Anywhere else — beside other statements, inside a
/// `fork`/`loop`/`on failure`/substep, or as a pipe stage — is an error.
fn check_placement(ctx: &mut Ctx<'_>, steps: &[Step]) {
    for step in steps {
        let alone = step.body.len() == 1;
        for statement in &step.body {
            if let Statement::Call(call) = statement {
                if ctx.subflows.contains_key(&call.call.name) && !alone {
                    ctx.error(
                        call.call.name_span,
                        format!(
                            "a subflow runs as its own step — `{}(…)` must be the only \
                             statement of step `{}`",
                            call.call.name, step.name
                        ),
                    );
                }
                continue;
            }
            check_nested(ctx, std::slice::from_ref(statement));
        }
        if let Some(on_failure) = &step.on_failure {
            check_nested(ctx, &on_failure.body);
        }
    }
}

/// Reject subflow invocations below the top level of a step body.
fn check_nested(ctx: &mut Ctx<'_>, statements: &[Statement]) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                if ctx.subflows.contains_key(&call.call.name) {
                    ctx.error(
                        call.call.name_span,
                        format!(
                            "a subflow runs as its own step — `{}(…)` cannot run inside \
                             another statement's block; give it its own top-level step",
                            call.call.name
                        ),
                    );
                }
            }
            Statement::Pipe(pipe) => {
                for stage in &pipe.stages {
                    if let PipeStage::Action { span, name } = stage
                        && ctx.subflows.contains_key(name)
                    {
                        ctx.error(
                            *span,
                            format!(
                                "`{name}` is a subflow — it is not a pipe stage; give it \
                                 its own step and bind its outcome"
                            ),
                        );
                    }
                }
            }
            Statement::Fork(fork) => check_nested(ctx, &fork.body),
            Statement::Loop(looped) => check_nested(ctx, &looped.body),
            Statement::SubStep(sub) => {
                check_nested(ctx, &sub.body);
                if let Some(on_failure) = &sub.on_failure {
                    check_nested(ctx, &on_failure.body);
                }
            }
            Statement::Spawn(_)
            | Statement::Wait(_)
            | Statement::Sleep(_)
            | Statement::Route(_)
            | Statement::Distribute(_)
            | Statement::Collect(_) => {}
        }
    }
}

/// Subflows compile inline: an invocation cycle (direct or mutual) can
/// never finish inlining, so it is rejected at the declaration.
fn check_recursion(ctx: &mut Ctx<'_>, subflows: &[SubflowDecl]) {
    // Three-color depth-first search for an invocation cycle.
    const WHITE: u8 = 0;
    const GRAY: u8 = 1;
    const BLACK: u8 = 2;
    fn visit(
        node: usize,
        calls: &[Vec<usize>],
        color: &mut [u8],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        color[node] = GRAY;
        stack.push(node);
        for &next in &calls[node] {
            if color[next] == GRAY {
                let start = stack.iter().position(|&member| member == next).unwrap_or(0);
                return Some(stack[start..].to_vec());
            }
            if color[next] == WHITE
                && let Some(cycle) = visit(next, calls, color, stack)
            {
                return Some(cycle);
            }
        }
        stack.pop();
        color[node] = BLACK;
        None
    }
    let index: BTreeMap<&str, usize> = subflows
        .iter()
        .enumerate()
        .map(|(position, decl)| (decl.name.as_str(), position))
        .collect();
    let mut calls: Vec<Vec<usize>> = vec![Vec::new(); subflows.len()];
    for (position, decl) in subflows.iter().enumerate() {
        let mut named = Vec::new();
        for step in &decl.steps {
            call_names(&step.body, &mut named);
            if let Some(on_failure) = &step.on_failure {
                call_names(&on_failure.body, &mut named);
            }
        }
        for name in named {
            if let Some(&target) = index.get(name.as_str()) {
                calls[position].push(target);
            }
        }
    }
    let mut color = vec![WHITE; subflows.len()];
    for node in 0..subflows.len() {
        if color[node] == WHITE
            && let Some(cycle) = visit(node, &calls, &mut color, &mut vec![])
        {
            let mut names: Vec<&str> = cycle
                .iter()
                .map(|&member| subflows[member].name.as_str())
                .collect();
            names.push(subflows[cycle[0]].name.as_str());
            ctx.error(
                subflows[cycle[0]].name_span,
                format!(
                    "subflow `{}` is invoked in terms of itself ({}) — subflows compile \
                     inline and cannot recurse",
                    subflows[cycle[0]].name,
                    names.join(" -> ")
                ),
            );
            return;
        }
    }
}

/// Every callee name written in a statement list, recursing into blocks.
fn call_names(statements: &[Statement], out: &mut Vec<String>) {
    for statement in statements {
        match statement {
            Statement::Call(call) => out.push(call.call.name.clone()),
            Statement::Spawn(spawn) => out.push(spawn.call.name.clone()),
            Statement::Pipe(pipe) => {
                for stage in &pipe.stages {
                    if let PipeStage::Action { name, .. } = stage {
                        out.push(name.clone());
                    }
                }
            }
            Statement::Fork(fork) => call_names(&fork.body, out),
            Statement::Loop(looped) => call_names(&looped.body, out),
            Statement::SubStep(sub) => {
                call_names(&sub.body, out);
                if let Some(on_failure) = &sub.on_failure {
                    call_names(&on_failure.body, out);
                }
            }
            Statement::Wait(_)
            | Statement::Sleep(_)
            | Statement::Route(_)
            | Statement::Distribute(_)
            | Statement::Collect(_) => {}
        }
    }
}
