use std::collections::BTreeSet;

use aion_awl::{Arg, Document, Expr, Guard, PipeEnd, PipeStage, Span, Statement, Step};

pub(super) fn rename_document(document: &mut Document, spans: &BTreeSet<(usize, usize)>, to: &str) {
    for step in &mut document.steps {
        rename_step(step, spans, to);
    }
}

fn matches_span(span: Span, spans: &BTreeSet<(usize, usize)>) -> bool {
    spans.contains(&(span.start, span.end))
}

fn rename_step(step: &mut Step, spans: &BTreeSet<(usize, usize)>, to: &str) {
    if matches_span(step.name_span, spans) {
        step.name.clone_from(&to.to_owned());
    }
    for after in &mut step.after {
        if matches_span(after.span, spans) {
            after.name.clone_from(&to.to_owned());
        }
    }
    rename_statements(&mut step.body, spans, to);
    if let Some(failure) = &mut step.on_failure {
        rename_statements(&mut failure.body, spans, to);
    }
    for outcome in &mut step.outcomes {
        if let Guard::When { expr, .. } = &mut outcome.guard {
            rename_expr(expr, spans, to);
        }
        rename_target(&mut outcome.route, spans, to);
    }
}

fn rename_statements(statements: &mut [Statement], spans: &BTreeSet<(usize, usize)>, to: &str) {
    for statement in statements {
        match statement {
            Statement::Call(call) => {
                for arg in &mut call.call.args {
                    rename_arg(arg, spans, to);
                }
                if let Some(bind) = &mut call.bind {
                    if matches_span(bind.span, spans) {
                        bind.name.clone_from(&to.to_owned());
                    }
                }
            }
            Statement::Spawn(spawn) => {
                for arg in &mut spawn.call.args {
                    rename_arg(arg, spans, to);
                }
            }
            Statement::Pipe(pipe) => {
                rename_expr(&mut pipe.head, spans, to);
                for stage in &mut pipe.stages {
                    if let PipeStage::Combinator(call) = stage {
                        if let Some(arg) = &mut call.arg {
                            rename_expr(arg, spans, to);
                        }
                    }
                }
                match &mut pipe.end {
                    PipeEnd::Bind(bind) if matches_span(bind.span, spans) => {
                        bind.name.clone_from(&to.to_owned());
                    }
                    PipeEnd::Bind(_) => {}
                    PipeEnd::Route(target) => rename_target(target, spans, to),
                }
            }
            Statement::Wait(wait) if matches_span(wait.bind.span, spans) => {
                wait.bind.name.clone_from(&to.to_owned());
            }
            Statement::Wait(_) | Statement::Sleep(_) => {}
            Statement::Fork(fork) => {
                if let aion_awl::ForkHeader::Collection {
                    var,
                    var_span,
                    collection,
                    ..
                } = &mut fork.header
                {
                    if matches_span(*var_span, spans) {
                        var.clone_from(&to.to_owned());
                    }
                    rename_expr(collection, spans, to);
                }
                rename_statements(&mut fork.body, spans, to);
                if let Some(bind) = &mut fork.join.bind {
                    if matches_span(bind.span, spans) {
                        bind.name.clone_from(&to.to_owned());
                    }
                }
            }
            Statement::Loop(looped) => {
                if matches_span(looped.var_span, spans) {
                    looped.var.clone_from(&to.to_owned());
                }
                rename_expr(&mut looped.seed, spans, to);
                if let Some(counter) = &mut looped.counter {
                    if matches_span(counter.span, spans) {
                        counter.name.clone_from(&to.to_owned());
                    }
                }
                rename_statements(&mut looped.body, spans, to);
                if let Some(until) = &mut looped.until {
                    rename_expr(&mut until.expr, spans, to);
                }
                if let Some(max) = &mut looped.max {
                    rename_expr(&mut max.expr, spans, to);
                }
            }
            Statement::Route(route) => rename_target(&mut route.target, spans, to),
            Statement::SubStep(step) => rename_step(step, spans, to),
        }
    }
}

fn rename_target(target: &mut aion_awl::RouteTarget, spans: &BTreeSet<(usize, usize)>, to: &str) {
    if matches_span(target.name_span, spans) {
        target.name.clone_from(&to.to_owned());
    }
    if let Some(payload) = &mut target.payload {
        for arg in payload {
            rename_arg(arg, spans, to);
        }
    }
}

fn rename_arg(arg: &mut Arg, spans: &BTreeSet<(usize, usize)>, to: &str) {
    rename_expr(&mut arg.value, spans, to);
}

fn rename_expr(expr: &mut Expr, spans: &BTreeSet<(usize, usize)>, to: &str) {
    match expr {
        Expr::Ref { span, name } if matches_span(*span, spans) => {
            name.clone_from(&to.to_owned());
        }
        Expr::List { items, .. } => {
            for item in items {
                rename_expr(item, spans, to);
            }
        }
        Expr::Record { args, .. } => {
            for arg in args {
                rename_arg(arg, spans, to);
            }
        }
        Expr::Field { base, .. } | Expr::Index { base, .. } => rename_expr(base, spans, to),
        Expr::Not { expr, .. } => rename_expr(expr, spans, to),
        Expr::Binary { left, right, .. } => {
            rename_expr(left, spans, to);
            rename_expr(right, spans, to);
        }
        Expr::Predicate { subject, .. } => rename_expr(subject, spans, to),
        Expr::CollectionPredicate {
            collection,
            predicate,
            ..
        } => {
            rename_expr(collection, spans, to);
            rename_expr(predicate, spans, to);
        }
        Expr::Ref { .. }
        | Expr::String { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_)
        | Expr::Variant { .. }
        | Expr::Workflow { .. }
        | Expr::Accessor { .. } => {}
    }
}
