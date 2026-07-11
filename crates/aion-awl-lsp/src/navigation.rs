use aion_awl::{Document, Span, Statement, Step, TypeBody, TypeRef};
use lsp_types::{Location, Position, Range, SymbolKind, Uri};
use serde::Serialize;

use crate::analysis::{byte_offset_at, range_for_span};

#[derive(Serialize)]
struct OutlineSymbol {
    name: String,
    kind: SymbolKind,
    range: Range,
    #[serde(rename = "selectionRange")]
    selection_range: Range,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<OutlineSymbol>,
}

impl OutlineSymbol {
    fn new(
        source: &str,
        name: &str,
        kind: SymbolKind,
        span: Span,
        name_span: Span,
        children: Vec<Self>,
    ) -> Self {
        Self {
            name: name.to_owned(),
            kind,
            range: range_for_span(source, span),
            selection_range: range_for_span(source, name_span),
            children,
        }
    }
}

pub(crate) fn document_symbols(source: &str) -> Option<serde_json::Value> {
    let document = aion_awl::parse(source).ok()?;
    let mut workflow_children = Vec::new();
    workflow_children.extend(document.types.iter().map(|declaration| {
        OutlineSymbol::new(
            source,
            &declaration.name,
            SymbolKind::STRUCT,
            declaration.span,
            declaration.name_span,
            Vec::new(),
        )
    }));
    workflow_children.extend(document.workers.iter().map(|worker| {
        let actions = worker
            .actions
            .iter()
            .map(|action| {
                OutlineSymbol::new(
                    source,
                    &action.name,
                    SymbolKind::FUNCTION,
                    action.span,
                    action.name_span,
                    Vec::new(),
                )
            })
            .collect();
        OutlineSymbol::new(
            source,
            &worker.name,
            SymbolKind::NAMESPACE,
            worker.span,
            worker.name_span,
            actions,
        )
    }));
    workflow_children.extend(document.steps.iter().map(|step| step_symbol(source, step)));
    let symbols = vec![OutlineSymbol::new(
        source,
        &document.name,
        SymbolKind::MODULE,
        document.span,
        document.name_span,
        workflow_children,
    )];
    serde_json::to_value(symbols).ok()
}

fn step_symbol(source: &str, step: &Step) -> OutlineSymbol {
    let children = step
        .body
        .iter()
        .filter_map(|statement| match statement {
            Statement::SubStep(substep) => Some(step_symbol(source, substep)),
            _ => None,
        })
        .collect();
    OutlineSymbol::new(
        source,
        &step.name,
        SymbolKind::METHOD,
        step.span,
        step.name_span,
        children,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NameKind {
    Action,
    Step,
    Type,
}

struct NameSite<'a> {
    name: &'a str,
    span: Span,
    kind: NameKind,
}

pub(crate) fn definition(source: &str, uri: &Uri, position: Position) -> Option<Location> {
    let document = aion_awl::parse(source).ok()?;
    let byte = byte_offset_at(source, position);
    let mut declarations = Vec::new();
    let mut references = Vec::new();
    collect_document(&document, &mut declarations, &mut references);
    let reference = references
        .into_iter()
        .find(|site| contains(site.span, byte))?;
    let mut matching = declarations
        .into_iter()
        .filter(|site| site.kind == reference.kind && site.name == reference.name);
    let declaration = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    Some(Location::new(
        uri.clone(),
        range_for_span(source, declaration.span),
    ))
}

fn contains(span: Span, byte: usize) -> bool {
    span.start <= byte && byte < span.end
}

fn collect_document<'a>(
    document: &'a Document,
    declarations: &mut Vec<NameSite<'a>>,
    references: &mut Vec<NameSite<'a>>,
) {
    for declaration in &document.types {
        declarations.push(NameSite {
            name: &declaration.name,
            span: declaration.name_span,
            kind: NameKind::Type,
        });
        collect_type_body(&declaration.body, references);
    }
    for input in &document.inputs {
        collect_type(&input.ty, references);
    }
    for signal in &document.signals {
        collect_type(&signal.ty, references);
    }
    for outcome in &document.outcomes {
        collect_type(&outcome.ty, references);
    }
    for worker in &document.workers {
        for action in &worker.actions {
            declarations.push(NameSite {
                name: &action.name,
                span: action.name_span,
                kind: NameKind::Action,
            });
            for parameter in &action.params {
                collect_type(&parameter.ty, references);
            }
            collect_type(&action.returns, references);
        }
    }
    for child in &document.children {
        declarations.push(NameSite {
            name: &child.name,
            span: child.name_span,
            kind: NameKind::Action,
        });
        for parameter in &child.params {
            collect_type(&parameter.ty, references);
        }
        collect_type(&child.returns, references);
    }
    for step in &document.steps {
        collect_step(step, declarations, references);
    }
}

fn collect_type_body<'a>(body: &'a TypeBody, references: &mut Vec<NameSite<'a>>) {
    if let TypeBody::Record { fields } = body {
        for field in fields {
            collect_type(&field.ty, references);
        }
    }
}

fn collect_type<'a>(ty: &'a TypeRef, references: &mut Vec<NameSite<'a>>) {
    match ty {
        TypeRef::Named { span, name } => references.push(NameSite {
            name,
            span: *span,
            kind: NameKind::Type,
        }),
        TypeRef::List { inner, .. } | TypeRef::Optional { inner, .. } => {
            collect_type(inner, references);
        }
    }
}

fn collect_step<'a>(
    step: &'a Step,
    declarations: &mut Vec<NameSite<'a>>,
    references: &mut Vec<NameSite<'a>>,
) {
    declarations.push(NameSite {
        name: &step.name,
        span: step.name_span,
        kind: NameKind::Step,
    });
    references.extend(step.after.iter().map(|after| NameSite {
        name: &after.name,
        span: after.span,
        kind: NameKind::Step,
    }));
    collect_statements(&step.body, declarations, references);
    if let Some(on_failure) = &step.on_failure {
        collect_statements(&on_failure.body, declarations, references);
    }
    references.extend(step.outcomes.iter().map(|outcome| NameSite {
        name: &outcome.route.name,
        span: outcome.route.name_span,
        kind: NameKind::Step,
    }));
}

fn collect_statements<'a>(
    statements: &'a [Statement],
    declarations: &mut Vec<NameSite<'a>>,
    references: &mut Vec<NameSite<'a>>,
) {
    for statement in statements {
        match statement {
            Statement::Call(call) => references.push(NameSite {
                name: &call.call.name,
                span: call.call.name_span,
                kind: NameKind::Action,
            }),
            Statement::Spawn(spawn) => references.push(NameSite {
                name: &spawn.call.name,
                span: spawn.call.name_span,
                kind: NameKind::Action,
            }),
            Statement::Pipe(pipe) => {
                for stage in &pipe.stages {
                    if let aion_awl::PipeStage::Action { span, name } = stage {
                        references.push(NameSite {
                            name,
                            span: *span,
                            kind: NameKind::Action,
                        });
                    }
                }
                if let aion_awl::PipeEnd::Route(target) = &pipe.end {
                    references.push(NameSite {
                        name: &target.name,
                        span: target.name_span,
                        kind: NameKind::Step,
                    });
                }
            }
            Statement::Fork(fork) => collect_statements(&fork.body, declarations, references),
            Statement::Loop(loop_statement) => {
                collect_statements(&loop_statement.body, declarations, references);
            }
            Statement::Route(route) => references.push(NameSite {
                name: &route.target.name,
                span: route.target.name_span,
                kind: NameKind::Step,
            }),
            Statement::SubStep(substep) => collect_step(substep, declarations, references),
            Statement::Wait(_) | Statement::Sleep(_) => {}
        }
    }
}
