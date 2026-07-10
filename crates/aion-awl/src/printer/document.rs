//! Canonical printing of documents: narration, header (with outcome
//! alignment), type declarations (with group alignment), workers, and
//! children. `parse ∘ print = id`; `print ∘ parse ∘ print = print`.

use crate::ast::{
    ActionDecl, ChildDecl, Comment, ConfigLine, DocLine, Document, FieldDecl, Lead, OutcomeDecl,
    ParamDecl, RouteDirection, TypeBody, TypeDecl, WorkerDecl,
};

use super::exprs::{duration_text, retry_text, type_ref_text, width};
use super::steps::print_step;

/// Maximum canonical line width in characters. Group-alignment padding is
/// exempt: the single-line-vs-multi-line decision is made on the unpadded
/// rendering.
pub(super) const MAX_WIDTH: usize = 100;

pub(super) struct Printer {
    pub(super) out: String,
}

impl Printer {
    pub(super) fn line(&mut self, indent: usize, text: &str, trailing: Option<&Comment>) {
        for _ in 0..indent {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        if let Some(comment) = trailing {
            self.out.push_str(" //");
            if !comment.text.is_empty() {
                self.out.push(' ');
                self.out.push_str(&comment.text);
            }
        }
        self.out.push('\n');
    }

    pub(super) fn leads(&mut self, indent: usize, leads: &[Lead]) {
        for lead in leads {
            match lead {
                Lead::Blank => self.out.push('\n'),
                Lead::Comment(comment) => {
                    let text = if comment.text.is_empty() {
                        "//".to_owned()
                    } else {
                        format!("// {}", comment.text)
                    };
                    self.line(indent, &text, None);
                }
            }
        }
    }

    pub(super) fn docs(&mut self, indent: usize, docs: &[DocLine]) {
        for doc in docs {
            self.line(indent, &format!("///{}", doc.text), None);
        }
    }
}

pub(super) fn print_document(printer: &mut Printer, document: &Document) {
    for line in &document.narration {
        printer.line(0, &format!("//!{}", line.text), None);
    }
    printer.leads(0, &document.lead);
    printer.line(
        0,
        &format!("workflow {}", document.name),
        document.trailing.as_ref(),
    );
    for input in &document.inputs {
        printer.leads(1, &input.lead);
        printer.line(
            1,
            &format!("input {}: {}", input.name, type_ref_text(&input.ty)),
            input.trailing.as_ref(),
        );
    }
    for signal in &document.signals {
        printer.leads(1, &signal.lead);
        printer.line(
            1,
            &format!("signal {}: {}", signal.name, type_ref_text(&signal.ty)),
            signal.trailing.as_ref(),
        );
    }
    print_outcome_decls(printer, &document.outcomes);
    print_type_decls(printer, &document.types);
    for worker in &document.workers {
        print_worker(printer, worker);
    }
    for child in &document.children {
        print_child(printer, child);
    }
    for step in &document.steps {
        print_step(printer, 0, step);
    }
    printer.leads(0, &document.epilogue);
}

/// Print header outcomes with group alignment: within a run of outcome
/// lines unbroken by blank lines or comments, the `type` and `route`
/// columns align.
fn print_outcome_decls(printer: &mut Printer, outcomes: &[OutcomeDecl]) {
    let mut index = 0;
    while index < outcomes.len() {
        let mut end = index + 1;
        while end < outcomes.len() && outcomes[end].lead.is_empty() {
            end += 1;
        }
        let group = &outcomes[index..end];
        let name_width = group
            .iter()
            .map(|outcome| width(&outcome.name) + 1)
            .max()
            .unwrap_or(0);
        let type_width = group
            .iter()
            .map(|outcome| width(&type_ref_text(&outcome.ty)) + 1)
            .max()
            .unwrap_or(0);
        for outcome in group {
            printer.leads(1, &outcome.lead);
            let name_part = pad(&format!("{}:", outcome.name), name_width);
            let type_part = pad(&format!("{},", type_ref_text(&outcome.ty)), type_width);
            let route = match outcome.direction {
                RouteDirection::Success => "success",
                RouteDirection::Failure => "failure",
            };
            printer.line(
                1,
                &format!("outcome {name_part} type {type_part} route {route}"),
                outcome.trailing.as_ref(),
            );
        }
        index = end;
    }
}

fn pad(text: &str, to: usize) -> String {
    let mut out = text.to_owned();
    while width(&out) < to {
        out.push(' ');
    }
    out
}

/// How a type declaration renders, for alignment grouping.
#[derive(PartialEq, Eq, Clone, Copy)]
enum TypeForm {
    /// Single-line `type Name { … }` — brace column aligns within a group.
    BraceLine,
    /// Single-line `type Name = …` — the `=` aligns within a group.
    EqualsLine,
    /// Multi-line rendering — never grouped.
    Block,
}

fn type_form(decl: &TypeDecl) -> TypeForm {
    match &decl.body {
        TypeBody::Record { fields } => {
            if record_renders_single_line(decl, fields) {
                TypeForm::BraceLine
            } else {
                TypeForm::Block
            }
        }
        TypeBody::Enum { .. } | TypeBody::SchemaImport { .. } => TypeForm::EqualsLine,
        TypeBody::SchemaInline { body, .. } => {
            if body.contains('\n') {
                TypeForm::Block
            } else {
                TypeForm::EqualsLine
            }
        }
    }
}

fn record_renders_single_line(decl: &TypeDecl, fields: &[FieldDecl]) -> bool {
    let plain = fields
        .iter()
        .all(|field| field.docs.is_empty() && field.lead.is_empty() && field.trailing.is_none());
    if !plain {
        return false;
    }
    width(&single_line_record(decl, fields, 0)) <= MAX_WIDTH
}

fn single_line_record(decl: &TypeDecl, fields: &[FieldDecl], name_width: usize) -> String {
    let fields_text: Vec<String> = fields
        .iter()
        .map(|field| format!("{}: {}", field.name, type_ref_text(&field.ty)))
        .collect();
    let name = pad(&decl.name, name_width.max(width(&decl.name)));
    if fields_text.is_empty() {
        format!("type {name} {{}}")
    } else {
        format!("type {name} {{ {} }}", fields_text.join(", "))
    }
}

fn equals_body_text(decl: &TypeDecl) -> String {
    match &decl.body {
        TypeBody::Enum { variants } => {
            let names: Vec<&str> = variants
                .iter()
                .map(|variant| variant.name.as_str())
                .collect();
            names.join(" | ")
        }
        TypeBody::SchemaImport { path, .. } => {
            format!("schema({})", super::exprs::string_literal(path))
        }
        TypeBody::SchemaInline { body, .. } => format!("schema {body}"),
        TypeBody::Record { .. } => String::new(),
    }
}

/// Print type declarations with group alignment: a maximal run of adjacent
/// single-line declarations of the same form (no blank lines, comments, or
/// doc lines between) pads names to a common column.
fn print_type_decls(printer: &mut Printer, types: &[TypeDecl]) {
    let mut index = 0;
    while index < types.len() {
        let form = type_form(&types[index]);
        let mut end = index + 1;
        if form != TypeForm::Block {
            while end < types.len()
                && types[end].lead.is_empty()
                && types[end].docs.is_empty()
                && type_form(&types[end]) == form
            {
                end += 1;
            }
        }
        let group = &types[index..end];
        let name_width = group
            .iter()
            .map(|decl| width(&decl.name))
            .max()
            .unwrap_or(0);
        for decl in group {
            print_type_decl(printer, decl, form, name_width);
        }
        index = end;
    }
}

fn print_type_decl(printer: &mut Printer, decl: &TypeDecl, form: TypeForm, name_width: usize) {
    printer.leads(0, &decl.lead);
    printer.docs(0, &decl.docs);
    match (&decl.body, form) {
        (TypeBody::Record { fields }, TypeForm::BraceLine) => {
            printer.line(
                0,
                &single_line_record(decl, fields, name_width),
                decl.trailing.as_ref(),
            );
        }
        (TypeBody::Record { fields }, _) => {
            printer.line(0, &format!("type {} {{", decl.name), None);
            for field in fields {
                printer.leads(1, &field.lead);
                printer.docs(1, &field.docs);
                printer.line(
                    1,
                    &format!("{}: {},", field.name, type_ref_text(&field.ty)),
                    field.trailing.as_ref(),
                );
            }
            printer.line(0, "}", decl.trailing.as_ref());
        }
        (_, TypeForm::EqualsLine) => {
            let name = pad(&decl.name, name_width);
            printer.line(
                0,
                &format!("type {name} = {}", equals_body_text(decl)),
                decl.trailing.as_ref(),
            );
        }
        (TypeBody::SchemaInline { body, .. }, _) => {
            // Multi-line inline schema: the body is verbatim, including its
            // own newlines and indentation.
            printer.line(
                0,
                &format!("type {} = schema {body}", decl.name),
                decl.trailing.as_ref(),
            );
        }
        _ => {}
    }
}

fn print_worker(printer: &mut Printer, worker: &WorkerDecl) {
    printer.leads(0, &worker.lead);
    printer.docs(0, &worker.docs);
    printer.line(
        0,
        &format!("worker {}", worker.name),
        worker.trailing.as_ref(),
    );
    for action in &worker.actions {
        print_action(printer, action);
    }
}

fn print_action(printer: &mut Printer, action: &ActionDecl) {
    printer.leads(1, &action.lead);
    printer.docs(1, &action.docs);
    printer.line(
        1,
        &format!(
            "action {}({}) -> {}",
            action.name,
            params_text(&action.params),
            type_ref_text(&action.returns)
        ),
        action.trailing.as_ref(),
    );
    if let Some(config) = &action.config {
        print_config_line(printer, 2, config);
    }
}

pub(super) fn print_config_line(printer: &mut Printer, indent: usize, config: &ConfigLine) {
    printer.leads(indent, &config.lead);
    let mut parts = Vec::new();
    if let Some(node) = &config.node {
        parts.push(format!("node {}", node.name));
    }
    if let Some(timeout) = &config.timeout {
        parts.push(format!("timeout {}", duration_text(timeout)));
    }
    if let Some(retry) = &config.retry {
        parts.push(retry_text(retry));
    }
    printer.line(indent, &parts.join(", "), config.trailing.as_ref());
}

fn print_child(printer: &mut Printer, child: &ChildDecl) {
    printer.leads(0, &child.lead);
    printer.docs(0, &child.docs);
    printer.line(
        0,
        &format!(
            "child {}({}) -> {}",
            child.name,
            params_text(&child.params),
            type_ref_text(&child.returns)
        ),
        child.trailing.as_ref(),
    );
}

fn params_text(params: &[ParamDecl]) -> String {
    let rendered: Vec<String> = params
        .iter()
        .map(|param| format!("{}: {}", param.name, type_ref_text(&param.ty)))
        .collect();
    rendered.join(", ")
}
