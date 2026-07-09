use crate::ast::{
    ActionDecl, ActionFieldTag, BinaryOp, CallExpr, CallTarget, Comment, Document, Expr,
    HandlerBlock, HandlerTerminal, IoDecl, RetrySpec, StepDecl, StepFieldTag, StepOp, TypeDecl,
    TypeRef,
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
        self.comments(0, &document.finish_leading);
        self.line(
            0,
            &format!("finish {}", expr(&document.finish)),
            document.finish_trailing.as_ref(),
        );
        self.comments(0, &document.epilogue_comments);
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
        self.description(0, decl.description_source.as_deref());
        let fields = decl
            .fields
            .iter()
            .map(|field| format!("{}: {}", field.name, ty(&field.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let single_line = format!("type {} {{ {} }}", decl.name, fields);
        let has_field_lines = decl.fields.iter().any(|field| {
            field.description.is_some()
                || !field.trivia.leading.is_empty()
                || field.trivia.trailing.is_some()
        });
        if single_line.chars().count() <= 100 && !has_field_lines {
            self.line(0, &single_line, decl.trivia.trailing.as_ref());
        } else {
            self.line(
                0,
                &format!("type {} {{", decl.name),
                decl.trivia.trailing.as_ref(),
            );
            for field in &decl.fields {
                self.comments(2, &field.trivia.leading);
                self.description(2, field.description_source.as_deref());
                self.line(
                    2,
                    &format!("{}: {},", field.name, ty(&field.ty)),
                    field.trivia.trailing.as_ref(),
                );
            }
            self.line(0, "}", None);
        }
    }
    fn description(&mut self, indent: usize, description: Option<&str>) {
        if let Some(description) = description {
            for line in description.split('\n') {
                let text = format!("///{line}");
                self.line(indent, &text, None);
            }
        }
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
            self.action_field_comments(decl, ActionFieldTag::Queue);
            let trailing = action_field_trailing(decl, ActionFieldTag::Queue);
            self.line(2, &format!("queue {}", string(queue)), trailing);
        }
        if let Some(node) = &decl.node {
            self.action_field_comments(decl, ActionFieldTag::Node);
            let trailing = action_field_trailing(decl, ActionFieldTag::Node);
            self.line(2, &format!("node {}", string(node)), trailing);
        }
        if let Some(timeout) = &decl.timeout {
            self.action_field_comments(decl, ActionFieldTag::Timeout);
            let trailing = action_field_trailing(decl, ActionFieldTag::Timeout);
            self.line(2, &format!("timeout {}", duration_text(timeout)), trailing);
        }
        if let Some(retry) = &decl.retry {
            self.action_field_comments(decl, ActionFieldTag::Retry);
            let trailing = action_field_trailing(decl, ActionFieldTag::Retry);
            self.line(2, &retry_text(retry), trailing);
        }
    }
    fn action_field_comments(&mut self, decl: &ActionDecl, tag: ActionFieldTag) {
        if let Some((_, comments)) = decl.leading_comments.iter().find(|(t, _)| *t == tag) {
            self.comments(2, comments);
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
            self.comments(2, &about.trivia.leading);
            self.line(
                2,
                &format!("about {}", about.text),
                about.trivia.trailing.as_ref(),
            );
        }
        if let Some(when) = &step.when {
            self.step_field_comments(step, StepFieldTag::When);
            let trailing = step_field_trailing(step, StepFieldTag::When);
            self.line(2, &format!("when {}", expr(when)), trailing);
        }
        if let Some(each) = &step.each {
            self.step_field_comments(step, StepFieldTag::Each);
            let trailing = step_field_trailing(step, StepFieldTag::Each);
            self.line(
                2,
                &format!("each {} in {}", each.name, expr(&each.in_expr)),
                trailing,
            );
        }
        self.step_field_comments(step, StepFieldTag::Op);
        let op_trailing = step_field_trailing(step, StepFieldTag::Op);
        match &step.op {
            StepOp::Do(call) => {
                self.line(2, &format!("do {}", call_target(call)), op_trailing);
            }
            StepOp::Wait { signal, .. } => {
                self.line(2, &format!("wait {signal}"), op_trailing);
            }
            StepOp::Sleep(duration) => {
                self.line(
                    2,
                    &format!("sleep {}", duration_text(duration)),
                    op_trailing,
                );
            }
        }
        if let Some(repeat) = &step.repeat {
            self.step_field_comments(step, StepFieldTag::Repeat);
            let trailing = step_field_trailing(step, StepFieldTag::Repeat);
            self.line(2, &format!("repeat up to {}", expr(repeat)), trailing);
        }
        if let Some(until) = &step.until {
            self.step_field_comments(step, StepFieldTag::Until);
            let trailing = step_field_trailing(step, StepFieldTag::Until);
            self.line(2, &format!("until {}", expr(until)), trailing);
        }
        if let Some(retry) = &step.retry {
            self.step_field_comments(step, StepFieldTag::Retry);
            let trailing = step_field_trailing(step, StepFieldTag::Retry);
            self.line(2, &retry_text(retry), trailing);
        }
        if let Some(timeout) = &step.timeout {
            self.step_field_comments(step, StepFieldTag::Timeout);
            let trailing = step_field_trailing(step, StepFieldTag::Timeout);
            self.line(2, &format!("timeout {}", duration_text(timeout)), trailing);
        }
        if let Some(handler) = &step.on_timeout {
            self.step_field_comments(step, StepFieldTag::OnTimeout);
            self.handler("timeout", handler);
        }
        if let Some(handler) = &step.on_failure {
            self.step_field_comments(step, StepFieldTag::OnFailure);
            self.handler("failure", handler);
        }
        if let Some(bind) = &step.bind_as {
            self.comments(2, &bind.trivia.leading);
            self.line(
                2,
                &format!("as {}", bind.name),
                bind.trivia.trailing.as_ref(),
            );
        }
        if let Some(queue) = &step.queue {
            self.step_field_comments(step, StepFieldTag::Queue);
            let trailing = step_field_trailing(step, StepFieldTag::Queue);
            self.line(2, &format!("queue {}", string(queue)), trailing);
        }
        if let Some(node) = &step.node {
            self.step_field_comments(step, StepFieldTag::Node);
            let trailing = step_field_trailing(step, StepFieldTag::Node);
            self.line(2, &format!("node {}", string(node)), trailing);
        }
    }
    fn step_field_comments(&mut self, step: &StepDecl, tag: StepFieldTag) {
        if let Some((_, comments)) = step.leading_comments.iter().find(|(t, _)| *t == tag) {
            self.comments(2, comments);
        }
    }
    fn handler(&mut self, kind: &str, handler: &HandlerBlock) {
        self.line(2, &format!("on {kind}"), None);
        for (idx, action) in handler.actions.iter().enumerate() {
            if let Some(comments) = handler.action_leading.get(idx) {
                self.comments(4, comments);
            }
            let trailing = handler.action_trailing.get(idx).and_then(Option::as_ref);
            self.line(4, &format!("do {}", call_target(action)), trailing);
        }
        self.comments(4, &handler.terminal_leading);
        let terminal_trailing = handler.terminal_trailing.as_ref();
        match &handler.terminal {
            HandlerTerminal::Finish(finish) => {
                self.line(4, &format!("finish {}", expr(finish)), terminal_trailing);
            }
            HandlerTerminal::Fail(_) => self.line(4, "fail", terminal_trailing),
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

fn step_field_trailing(step: &StepDecl, tag: StepFieldTag) -> Option<&Comment> {
    step.trailing_comments
        .iter()
        .find(|(t, _)| *t == tag)
        .map(|(_, comment)| comment)
}
fn action_field_trailing(decl: &ActionDecl, tag: ActionFieldTag) -> Option<&Comment> {
    decl.trailing_comments
        .iter()
        .find(|(t, _)| *t == tag)
        .map(|(_, comment)| comment)
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
