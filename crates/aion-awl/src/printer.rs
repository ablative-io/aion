#![allow(missing_docs)]

use crate::ast::{
    ActionDecl, BinaryOp, CallExpr, CallTarget, Comment, Document, Expr, HandlerBlock,
    HandlerTerminal, IoDecl, RetrySpec, StepDecl, StepOp, TypeDecl, TypeRef,
};
use crate::parser::duration_text;

/// Render an AWL document in the canonical AWL-0 format.
#[must_use]
pub fn print(document: &Document) -> String {
    let mut p = Printer { out: String::new() };
    p.document(document);
    p.out
}

struct Printer {
    out: String,
}

impl Printer {
    fn document(&mut self, document: &Document) {
        self.comments(0, &document.workflow.trivia.leading);
        self.line(
            0,
            &format!("workflow {}", document.workflow.name),
            document.workflow.trivia.trailing.as_ref(),
        );
        if let Some(about) = &document.about {
            self.comments(0, &about.trivia.leading);
            self.line(
                0,
                &format!("about {}", about.text),
                about.trivia.trailing.as_ref(),
            );
        }
        self.blank();
        self.group_io("input", &document.inputs);
        if let Some(output) = &document.output {
            self.io("output", output);
        }
        if let Some(error) = &document.error {
            self.io("error", error);
        }
        if !document.signals.is_empty()
            || !document.types.is_empty()
            || !document.actions.is_empty()
            || !document.steps.is_empty()
        {
            self.blank();
        }
        self.group_io("signal", &document.signals);
        if !document.types.is_empty() && (!document.signals.is_empty()) {
            self.blank();
        }
        for ty in &document.types {
            self.type_decl(ty);
        }
        if !document.actions.is_empty() {
            self.blank();
        }
        for action in &document.actions {
            self.action(action);
        }
        if !document.steps.is_empty() {
            self.blank();
        }
        for (idx, step) in document.steps.iter().enumerate() {
            if idx > 0 {
                self.blank();
            }
            self.step(step);
        }
        if !document.steps.is_empty() {
            self.blank();
        }
        self.line(0, &format!("finish {}", expr(&document.finish)), None);
    }

    fn group_io(&mut self, keyword: &str, decls: &[IoDecl]) {
        for decl in decls {
            self.io(keyword, decl);
        }
    }
    fn io(&mut self, keyword: &str, decl: &IoDecl) {
        self.comments(0, &decl.trivia.leading);
        let code = if decl.name.is_empty() {
            format!("{keyword} {}", ty(&decl.ty))
        } else {
            format!("{keyword} {}: {}", decl.name, ty(&decl.ty))
        };
        self.line(0, &code, decl.trivia.trailing.as_ref());
    }
    fn type_decl(&mut self, decl: &TypeDecl) {
        self.comments(0, &decl.trivia.leading);
        let fields = decl
            .fields
            .iter()
            .map(|field| format!("{}: {}", field.name, ty(&field.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        self.line(
            0,
            &format!("type {} {{ {} }}", decl.name, fields),
            decl.trivia.trailing.as_ref(),
        );
    }
    fn action(&mut self, decl: &ActionDecl) {
        self.comments(0, &decl.trivia.leading);
        let params = decl
            .params
            .iter()
            .map(|field| format!("{}: {}", field.name, ty(&field.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        self.line(
            0,
            &format!("action {}({}) -> {}", decl.name, params, ty(&decl.returns)),
            decl.trivia.trailing.as_ref(),
        );
        if let Some(queue) = &decl.queue {
            self.line(2, &format!("queue {}", string(queue)), None);
        }
        if let Some(node) = &decl.node {
            self.line(2, &format!("node {}", string(node)), None);
        }
        if let Some(timeout) = &decl.timeout {
            self.line(2, &format!("timeout {}", duration_text(timeout)), None);
        }
        if let Some(retry) = &decl.retry {
            self.line(2, &retry_text(retry), None);
        }
    }
    fn step(&mut self, step: &StepDecl) {
        self.comments(0, &step.trivia.leading);
        self.line(
            0,
            &format!("step {}", step.name),
            step.trivia.trailing.as_ref(),
        );
        if let Some(about) = &step.about {
            self.line(
                2,
                &format!("about {}", about.text),
                about.trivia.trailing.as_ref(),
            );
        }
        if let Some(when) = &step.when {
            self.line(2, &format!("when {}", expr(when)), None);
        }
        if let Some(each) = &step.each {
            self.line(
                2,
                &format!("each {} in {}", each.name, expr(&each.in_expr)),
                None,
            );
        }
        match &step.op {
            StepOp::Do(call) => self.line(2, &format!("do {}", call_target(call)), None),
            StepOp::Wait { signal, .. } => self.line(2, &format!("wait {signal}"), None),
            StepOp::Sleep(duration) => {
                self.line(2, &format!("sleep {}", duration_text(duration)), None);
            }
        }
        if let Some(repeat) = &step.repeat {
            self.line(2, &format!("repeat up to {}", expr(repeat)), None);
        }
        if let Some(until) = &step.until {
            self.line(2, &format!("until {}", expr(until)), None);
        }
        if let Some(retry) = &step.retry {
            self.line(2, &retry_text(retry), None);
        }
        if let Some(timeout) = &step.timeout {
            self.line(2, &format!("timeout {}", duration_text(timeout)), None);
        }
        if let Some(handler) = &step.on_timeout {
            self.handler("timeout", handler);
        }
        if let Some(handler) = &step.on_failure {
            self.handler("failure", handler);
        }
        if let Some(bind) = &step.bind_as {
            self.line(
                2,
                &format!("as {}", bind.name),
                bind.trivia.trailing.as_ref(),
            );
        }
        if let Some(queue) = &step.queue {
            self.line(2, &format!("queue {}", string(queue)), None);
        }
        if let Some(node) = &step.node {
            self.line(2, &format!("node {}", string(node)), None);
        }
    }
    fn handler(&mut self, kind: &str, handler: &HandlerBlock) {
        self.line(2, &format!("on {kind}"), None);
        for action in &handler.actions {
            self.line(4, &format!("do {}", call_target(action)), None);
        }
        match &handler.terminal {
            HandlerTerminal::Finish(finish) => {
                self.line(4, &format!("finish {}", expr(finish)), None);
            }
            HandlerTerminal::Fail(_) => self.line(4, "fail", None),
        }
    }
    fn comments(&mut self, indent: usize, comments: &[Comment]) {
        for comment in comments {
            self.line(indent, &format!("// {}", comment.text), None);
        }
    }
    fn line(&mut self, indent: usize, code: &str, trailing: Option<&Comment>) {
        self.out.push_str(&" ".repeat(indent));
        self.out.push_str(code);
        if let Some(comment) = trailing {
            self.out.push_str("  // ");
            self.out.push_str(&comment.text);
        }
        self.out.push('\n');
    }
    fn blank(&mut self) {
        if !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }
}

fn ty(type_ref: &TypeRef) -> String {
    match type_ref {
        TypeRef::Named { name, .. } => name.clone(),
        TypeRef::List { inner, .. } => format!("List({})", ty(inner)),
        TypeRef::Option { inner, .. } => format!("Option({})", ty(inner)),
    }
}
fn retry_text(retry: &RetrySpec) -> String {
    match retry {
        RetrySpec::Every { count, every, .. } => {
            format!("retry {count} every {}", duration_text(every))
        }
        RetrySpec::Backoff {
            count, min, max, ..
        } => format!(
            "retry {count} backoff {}..{}",
            duration_text(min),
            duration_text(max)
        ),
    }
}
fn call_target(call: &CallTarget) -> String {
    match call {
        CallTarget::Action(call) => call_expr(call),
        CallTarget::Child { workflow, args, .. } => format!(
            "child {}({})",
            workflow,
            args.iter().map(expr).collect::<Vec<_>>().join(", ")
        ),
    }
}
fn call_expr(call: &CallExpr) -> String {
    format!(
        "{}({})",
        call.name,
        call.args.iter().map(expr).collect::<Vec<_>>().join(", ")
    )
}
fn expr(node: &Expr) -> String {
    match node {
        Expr::String { value, .. } => string(value),
        Expr::Int { value, .. } => value.to_string(),
        Expr::Float { value, .. } => value.clone(),
        Expr::Bool { value, .. } => value.to_string(),
        Expr::Duration(duration) => duration_text(duration),
        Expr::List { items, .. } => format!(
            "[{}]",
            items.iter().map(expr).collect::<Vec<_>>().join(", ")
        ),
        Expr::Ref { name, .. } => name.clone(),
        Expr::Field { base, field, .. } => format!("{}.{}", expr_atom(base), field),
        Expr::Record { name, fields, .. } => format!(
            "{}({})",
            name,
            fields
                .iter()
                .map(|field| format!("{}: {}", field.name, expr(&field.value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Not { expr: inner, .. } => format!("not {}", expr_atom(inner)),
        Expr::Binary {
            left, op, right, ..
        } => format!("{} {} {}", expr_atom(left), op_text(*op), expr_atom(right)),
    }
}
fn expr_atom(expr: &Expr) -> String {
    match expr {
        Expr::Binary { .. } => format!("({})", self::expr(expr)),
        _ => self::expr(expr),
    }
}
fn op_text(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Or => "or",
        BinaryOp::And => "and",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::Add => "+",
    }
}
fn string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}
