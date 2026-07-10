//! Canonical printing of steps: bodies in written order, the pipe-chain
//! break rule, and the outcome-clause layout (break after the guard comma,
//! greedy payload wrapping) — all at the 100-column rule.

use std::fmt::Write as _;

use crate::ast::{
    CombinatorKind, ForkHeader, ForkStmt, Guard, LoopStmt, OutcomeClause, PipeEnd, PipeStage,
    RouteTarget, Statement, Step,
};

use super::document::{MAX_WIDTH, Printer, print_config_line};
use super::exprs::{arg_text, args_text, duration_text, expr_text, width};

pub(super) fn print_step(printer: &mut Printer, indent: usize, step: &Step) {
    printer.leads(indent, &step.lead);
    printer.docs(indent, &step.docs);
    let mut header = format!("step {}", step.name);
    if !step.after.is_empty() {
        let names: Vec<&str> = step.after.iter().map(|dep| dep.name.as_str()).collect();
        let _ = write!(header, " after {}", names.join(", "));
    }
    printer.line(indent, &header, step.trailing.as_ref());
    for statement in &step.body {
        print_statement(printer, indent + 1, statement);
    }
    if let Some(on_failure) = &step.on_failure {
        printer.leads(indent + 1, &on_failure.lead);
        printer.line(indent + 1, "on failure", on_failure.trailing.as_ref());
        for statement in &on_failure.body {
            print_statement(printer, indent + 2, statement);
        }
    }
    for outcome in &step.outcomes {
        print_outcome_clause(printer, indent + 1, outcome);
    }
}

fn print_statement(printer: &mut Printer, indent: usize, statement: &Statement) {
    match statement {
        Statement::Call(call) => {
            printer.leads(indent, &call.lead);
            let mut text = format!("{}({})", call.call.name, args_text(&call.call.args));
            if let Some(bind) = &call.bind {
                let _ = write!(text, " -> {}", bind.name);
            }
            printer.line(indent, &text, call.trailing.as_ref());
            if let Some(config) = &call.config {
                print_config_line(printer, indent + 1, config);
            }
        }
        Statement::Spawn(spawn) => {
            printer.leads(indent, &spawn.lead);
            let mut text = format!("spawn {}({})", spawn.call.name, args_text(&spawn.call.args));
            if let Some(bind) = &spawn.bind {
                let _ = write!(text, " -> {}", bind.name);
            }
            printer.line(indent, &text, spawn.trailing.as_ref());
        }
        Statement::Pipe(pipe) => {
            printer.leads(indent, &pipe.lead);
            print_pipe(printer, indent, pipe);
        }
        Statement::Wait(wait) => {
            printer.leads(indent, &wait.lead);
            let mut text = format!("wait {}", wait.signal);
            if let Some(timeout) = &wait.timeout {
                let _ = write!(text, " timeout {}", duration_text(timeout));
            }
            let _ = write!(text, " -> {}", wait.bind.name);
            printer.line(indent, &text, wait.trailing.as_ref());
        }
        Statement::Sleep(sleep) => {
            printer.leads(indent, &sleep.lead);
            printer.line(
                indent,
                &format!("sleep {}", duration_text(&sleep.duration)),
                sleep.trailing.as_ref(),
            );
        }
        Statement::Fork(fork) => print_fork(printer, indent, fork),
        Statement::Loop(loop_stmt) => print_loop(printer, indent, loop_stmt),
        Statement::Route(route) => {
            printer.leads(indent, &route.lead);
            printer.line(
                indent,
                &format!("route {}", route_target_text(&route.target)),
                route.trailing.as_ref(),
            );
        }
        Statement::SubStep(step) => print_step(printer, indent, step),
    }
}

fn print_fork(printer: &mut Printer, indent: usize, fork: &ForkStmt) {
    printer.leads(indent, &fork.lead);
    let header = match &fork.header {
        ForkHeader::Named => "fork".to_owned(),
        ForkHeader::Collection {
            var,
            collection,
            sequential,
            ..
        } => {
            let mut text = format!("fork {var} in {}", expr_text(collection));
            if *sequential {
                text.push_str(" sequential");
            }
            text
        }
    };
    printer.line(indent, &header, fork.trailing.as_ref());
    for branch in &fork.body {
        print_statement(printer, indent + 1, branch);
    }
    printer.leads(indent, &fork.join.lead);
    let join = match &fork.join.bind {
        Some(bind) => format!("join -> {}", bind.name),
        None => "join".to_owned(),
    };
    printer.line(indent, &join, fork.join.trailing.as_ref());
}

fn print_loop(printer: &mut Printer, indent: usize, loop_stmt: &LoopStmt) {
    printer.leads(indent, &loop_stmt.lead);
    let mut header = format!("loop {} = {}", loop_stmt.var, expr_text(&loop_stmt.seed));
    if let Some(counter) = &loop_stmt.counter {
        let _ = write!(header, " counting {}", counter.name);
    }
    printer.line(indent, &header, loop_stmt.trailing.as_ref());
    for inner in &loop_stmt.body {
        print_statement(printer, indent + 1, inner);
    }
    if let Some(until) = &loop_stmt.until {
        printer.leads(indent + 1, &until.lead);
        printer.line(
            indent + 1,
            &format!("until {}", expr_text(&until.expr)),
            until.trailing.as_ref(),
        );
    }
    if let Some(max) = &loop_stmt.max {
        printer.leads(indent + 1, &max.lead);
        printer.line(
            indent + 1,
            &format!("max {}", expr_text(&max.expr)),
            max.trailing.as_ref(),
        );
    }
}

fn print_pipe(printer: &mut Printer, indent: usize, pipe: &crate::ast::PipeStmt) {
    let head = expr_text(&pipe.head);
    let stages: Vec<String> = pipe.stages.iter().map(stage_text).collect();
    let mut one_line = head.clone();
    for stage in &stages {
        let _ = write!(one_line, " |> {stage}");
    }
    match &pipe.end {
        PipeEnd::Bind(bind) => {
            let _ = write!(one_line, " -> {}", bind.name);
        }
        PipeEnd::Route(target) => {
            let _ = write!(one_line, " |> route {}", route_target_text(target));
        }
    }
    // A stage-less bind (`head -> name`) has no `|>` to break before — the
    // break rule is "break before each `|>`" — so it stays on one line at
    // any width; a wrapped `-> name` continuation would not re-parse.
    let stageless_bind = stages.is_empty() && matches!(&pipe.end, PipeEnd::Bind(_));
    if stageless_bind || indent * 2 + width(&one_line) <= MAX_WIDTH {
        printer.line(indent, &one_line, pipe.trailing.as_ref());
        return;
    }
    // A chain longer than the column limit breaks before each `|>` with one
    // extra indent; a `-> name` binding stays on the last stage's line and
    // that line carries the chain's trailing comment.
    printer.line(indent, &head, None);
    let last = stages.len().saturating_sub(1);
    for (position, stage) in stages.iter().enumerate() {
        let mut text = format!("|> {stage}");
        let mut trailing = None;
        if position == last {
            if let PipeEnd::Bind(bind) = &pipe.end {
                let _ = write!(text, " -> {}", bind.name);
                trailing = pipe.trailing.as_ref();
            }
        }
        printer.line(indent + 1, &text, trailing);
    }
    if let PipeEnd::Route(target) = &pipe.end {
        printer.line(
            indent + 1,
            &format!("|> route {}", route_target_text(target)),
            pipe.trailing.as_ref(),
        );
    }
}

fn stage_text(stage: &PipeStage) -> String {
    match stage {
        PipeStage::Action { name, .. } => name.clone(),
        PipeStage::Field { name, .. } => format!(".{name}"),
        PipeStage::Combinator(combinator) => {
            let name = match combinator.kind {
                CombinatorKind::Filter => "filter",
                CombinatorKind::Map => "map",
                CombinatorKind::Sort => "sort",
                CombinatorKind::Count => "count",
            };
            match &combinator.arg {
                Some(arg) => format!("{name}({})", expr_text(arg)),
                None => name.to_owned(),
            }
        }
    }
}

fn route_target_text(target: &RouteTarget) -> String {
    match &target.payload {
        Some(args) => format!("{}({})", target.name, args_text(args)),
        None => target.name.clone(),
    }
}

fn guard_text(guard: &Guard) -> String {
    match guard {
        Guard::When { expr, .. } => format!("when {}", expr_text(expr)),
        Guard::Otherwise { .. } => "otherwise".to_owned(),
    }
}

/// Print one outcome clause. A bare route stays on the guard's line when
/// the clause fits; a payload-constructing route ALWAYS breaks after the
/// guard comma (the spec's worked examples break `route out(...)` clauses
/// even under 100 columns — byte-identity with the flagship pins the
/// examples' reading over the printer-contract prose). The payload wraps
/// greedily one level deeper when the route line itself overflows.
fn print_outcome_clause(printer: &mut Printer, indent: usize, outcome: &OutcomeClause) {
    printer.leads(indent, &outcome.lead);
    let opening = format!("outcome {}: {}", outcome.name, guard_text(&outcome.guard));
    let route = route_target_text(&outcome.route);
    let one_line = format!("{opening}, route {route}");
    if outcome.route.payload.is_none() && indent * 2 + width(&one_line) <= MAX_WIDTH {
        printer.line(indent, &one_line, outcome.trailing.as_ref());
        return;
    }
    printer.line(indent, &format!("{opening},"), None);
    let route_line = format!("route {route}");
    if (indent + 1) * 2 + width(&route_line) <= MAX_WIDTH {
        printer.line(indent + 1, &route_line, outcome.trailing.as_ref());
        return;
    }
    print_wrapped_route(printer, indent + 1, outcome);
}

/// Greedy payload wrap: pack arguments onto the `route` line while they
/// fit, continuing one level deeper.
fn print_wrapped_route(printer: &mut Printer, indent: usize, outcome: &OutcomeClause) {
    let Some(args) = &outcome.route.payload else {
        printer.line(
            indent,
            &format!("route {}", outcome.route.name),
            outcome.trailing.as_ref(),
        );
        return;
    };
    let mut line = format!("route {}(", outcome.route.name);
    let mut line_indent = indent;
    let last = args.len().saturating_sub(1);
    for (position, arg) in args.iter().enumerate() {
        let piece = format!(
            "{}{}",
            arg_text(arg),
            if position == last { ")" } else { "," }
        );
        let separator = if line.ends_with('(') { "" } else { " " };
        let candidate = format!("{line}{separator}{piece}");
        if line_indent * 2 + width(&candidate) <= MAX_WIDTH {
            line = candidate;
        } else {
            printer.line(line_indent, &line, None);
            line_indent = indent + 1;
            line = piece;
        }
    }
    printer.line(line_indent, &line, outcome.trailing.as_ref());
}
