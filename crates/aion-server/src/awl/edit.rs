use std::collections::BTreeSet;

use super::handlers::{CheckRequest, check_source};
use aion_awl::{Document, PipeEnd, Statement, Step, semantic};

pub use super::edit_types::*;

pub fn edit_source(request: &EditRequest) -> EditResponse {
    let mut document = match aion_awl::parse(&request.source) {
        Ok(document) => document,
        Err(error) => return refused(RefusalCode::InvalidSource, error.message),
    };
    let rename = match apply_operation(&mut document, &request.operation) {
        Ok(rename) => rename,
        Err(error) => return EditResponse::from_refusal(error),
    };
    let source = aion_awl::print(&document);
    let reparsed = match aion_awl::parse(&source) {
        Ok(document) => document,
        Err(error) => return refused(RefusalCode::CanonicalizationFailed, error.message),
    };
    let canonical = aion_awl::print(&reparsed);
    if canonical != source {
        return refused(
            RefusalCode::CanonicalizationFailed,
            "canonical printer was not idempotent".to_owned(),
        );
    }
    let checked = check_source(&CheckRequest {
        source: source.clone(),
        path: None,
    });
    EditResponse {
        ok: true,
        source: Some(source),
        diagnostics: checked.diagnostics,
        refusal: None,
        rename,
    }
}

fn apply_operation(
    document: &mut Document,
    operation: &EditOperation,
) -> EditResult<Option<RenameMapping>> {
    match operation {
        EditOperation::AddStep { name, prose } => add_step(document, name, prose),
        EditOperation::AddAction {
            worker,
            name,
            params,
            return_type,
        } => add_action(document, worker, name, params, return_type),
        EditOperation::AddOutcomeRoute {
            source,
            target,
            name,
            guard,
            payload,
        } => add_outcome_route(document, source, target, name, guard, payload),
        EditOperation::AddFallThrough { source, target } => {
            add_fall_through(document, source, target)
        }
        EditOperation::EditProse { step, prose } => edit_prose(document, step, prose),
        EditOperation::RenameBinding { kind, from, to } => {
            rename_binding(document, *kind, from, to)
        }
        EditOperation::DeleteStep { step } => delete_step(document, step),
    }
}

fn add_step(document: &mut Document, name: &str, prose: &str) -> EditResult<Option<RenameMapping>> {
    if document.steps.iter().any(|step| step.name == name) {
        return Err(refusal(
            RefusalCode::NameCollision,
            "step name already exists",
        ));
    }
    let mut fragment = parse_fragment(&format!("{}step {name}\n", docs(prose)))?;
    let step = fragment.steps.pop().ok_or_else(|| {
        refusal(
            RefusalCode::InvalidOperation,
            "step could not be constructed",
        )
    })?;
    document.steps.push(step);
    Ok(None)
}

fn add_action(
    document: &mut Document,
    worker_name: &str,
    name: &str,
    params: &[ActionParameter],
    return_type: &str,
) -> EditResult<Option<RenameMapping>> {
    let worker = document
        .workers
        .iter_mut()
        .find(|worker| worker.name == worker_name)
        .ok_or_else(|| refusal(RefusalCode::UnknownWorker, "worker does not exist"))?;
    if worker.actions.iter().any(|action| action.name == name) {
        return Err(refusal(
            RefusalCode::NameCollision,
            "action name already exists",
        ));
    }
    let parameters = params
        .iter()
        .map(|parameter| format!("{}: {}", parameter.name, parameter.ty))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!("worker gesture_worker\n  action {name}({parameters}) -> {return_type}\n");
    let mut fragment = parse_fragment(&source)?;
    let action = fragment
        .workers
        .pop()
        .and_then(|mut worker| worker.actions.pop())
        .ok_or_else(|| {
            refusal(
                RefusalCode::InvalidOperation,
                "action could not be constructed",
            )
        })?;
    worker.actions.push(action);
    Ok(None)
}

fn add_outcome_route(
    document: &mut Document,
    source: &str,
    target: &str,
    name: &str,
    guard: &RouteGuard,
    payload: &[RouteArgument],
) -> EditResult<Option<RenameMapping>> {
    if !document.steps.iter().any(|step| step.name == target)
        && !document
            .outcomes
            .iter()
            .any(|outcome| outcome.name == target)
    {
        return Err(refusal(
            RefusalCode::InvalidRouteTarget,
            "route target does not exist",
        ));
    }
    let source_step = document
        .steps
        .iter_mut()
        .find(|step| step.name == source)
        .ok_or_else(|| refusal(RefusalCode::UnknownStep, "source step does not exist"))?;
    if source_step
        .outcomes
        .iter()
        .any(|outcome| outcome.name == name)
    {
        return Err(refusal(
            RefusalCode::NameCollision,
            "outcome arm name already exists",
        ));
    }
    let guard_text = match guard {
        RouteGuard::When { expression } => format!("when {expression}"),
        RouteGuard::Otherwise => "otherwise".to_owned(),
    };
    let payload_text = if payload.is_empty() {
        String::new()
    } else {
        let arguments = payload
            .iter()
            .map(|argument| format!("{}: {}", argument.name, argument.expression))
            .collect::<Vec<_>>()
            .join(", ");
        format!("({arguments})")
    };
    let source = format!(
        "step gesture_step\n  outcome {name}: {guard_text}, route {target}{payload_text}\n"
    );
    let mut fragment = parse_fragment(&source)?;
    let outcome = fragment
        .steps
        .pop()
        .and_then(|mut step| step.outcomes.pop())
        .ok_or_else(|| {
            refusal(
                RefusalCode::InvalidOperation,
                "route could not be constructed",
            )
        })?;
    source_step.outcomes.push(outcome);
    Ok(None)
}

fn add_fall_through(
    document: &mut Document,
    source: &str,
    target: &str,
) -> EditResult<Option<RenameMapping>> {
    let source_index = step_index(document, source)?;
    let target_index = step_index(document, target)?;
    if source == target
        || !document.steps[target_index].after.is_empty()
        || !falls_through(&document.steps[source_index])
    {
        return Err(refusal(
            RefusalCode::FallThroughUnavailable,
            "steps cannot form an implicit fall-through edge",
        ));
    }
    let target_step = document.steps.remove(target_index);
    let adjusted_source = if target_index < source_index {
        source_index - 1
    } else {
        source_index
    };
    document.steps.insert(adjusted_source + 1, target_step);
    let projected = super::projection::build(document);
    let has_edge = projected.edges.iter().any(|edge| {
        edge.source == source
            && edge.target == target
            && matches!(
                edge.kind,
                super::projection::ProjectionEdgeKind::FallThrough
            )
    });
    if !has_edge {
        return Err(refusal(
            RefusalCode::FallThroughUnavailable,
            "the target is already controlled by an explicit route",
        ));
    }
    Ok(None)
}

fn edit_prose(
    document: &mut Document,
    name: &str,
    prose: &str,
) -> EditResult<Option<RenameMapping>> {
    let docs = parse_fragment(&format!("{}step gesture_step\n", docs(prose)))?
        .steps
        .pop()
        .map(|step| step.docs)
        .ok_or_else(|| {
            refusal(
                RefusalCode::InvalidOperation,
                "prose could not be constructed",
            )
        })?;
    let step = document
        .steps
        .iter_mut()
        .find(|step| step.name == name)
        .ok_or_else(|| refusal(RefusalCode::UnknownStep, "step does not exist"))?;
    step.docs = docs;
    Ok(None)
}

fn delete_step(document: &mut Document, name: &str) -> EditResult<Option<RenameMapping>> {
    let index = step_index(document, name)?;
    let target_span = document.steps[index].name_span;
    let analysis = semantic::analyze(document);
    let referenced = analysis.iter().any(|info| {
        info.span != target_span
            && info
                .declaration
                .as_ref()
                .is_some_and(|declaration| declaration.span == target_span)
    });
    if referenced {
        return Err(refusal(
            RefusalCode::StepInUse,
            "step is referenced by the workflow",
        ));
    }
    document.steps.remove(index);
    Ok(None)
}

fn rename_binding(
    document: &mut Document,
    kind: RenameKind,
    from: &str,
    to: &str,
) -> EditResult<Option<RenameMapping>> {
    let analysis = semantic::analyze(document);
    if !analysis.diagnostics().is_empty() {
        return Err(refusal(
            RefusalCode::InvalidSource,
            "semantic rename requires a check-clean source document",
        ));
    }
    let declaration_kind = match kind {
        RenameKind::Step => semantic::DeclarationKind::Step,
        RenameKind::Binding => semantic::DeclarationKind::Binding,
    };
    let declarations = analysis
        .iter()
        .filter_map(|info| {
            info.declaration
                .as_ref()
                .filter(|declaration| info.span == declaration.span)
        })
        .filter(|declaration| declaration.kind == declaration_kind && declaration.name == from)
        .collect::<Vec<_>>();
    if declarations.len() != 1 {
        return Err(refusal(
            if declarations.is_empty() {
                RefusalCode::UnknownBinding
            } else {
                RefusalCode::NameCollision
            },
            "rename source is missing or ambiguous",
        ));
    }
    if analysis
        .iter()
        .filter_map(|info| info.declaration.as_ref())
        .any(|declaration| {
            declaration.kind == declaration_kind
                && declaration.name == to
                && declaration.span != declarations[0].span
        })
    {
        return Err(refusal(
            RefusalCode::NameCollision,
            "rename target collides with a declaration",
        ));
    }
    let target = declarations[0].span;
    let spans: BTreeSet<_> = analysis
        .iter()
        .filter(|info| {
            info.declaration
                .as_ref()
                .is_some_and(|declaration| declaration.span == target)
        })
        .map(|info| (info.span.start, info.span.end))
        .collect();
    super::edit_rename::rename_document(document, &spans, to);
    let canonical = aion_awl::print(document);
    let reparsed = aion_awl::parse(&canonical)
        .map_err(|error| refusal(RefusalCode::InvalidOperation, error.message))?;
    if !semantic::analyze(&reparsed).diagnostics().is_empty() {
        return Err(refusal(
            RefusalCode::NameCollision,
            "rename would make the document invalid",
        ));
    }
    Ok(Some(RenameMapping {
        kind,
        from: from.to_owned(),
        to: to.to_owned(),
    }))
}

fn parse_fragment(body: &str) -> EditResult<Document> {
    let source = format!(
        "//! Projectional edit fragment.\nworkflow gesture_fragment\n  outcome done: type String, route success\n\n{body}"
    );
    aion_awl::parse(&source).map_err(|error| refusal(RefusalCode::InvalidOperation, error.message))
}

fn docs(prose: &str) -> String {
    let mut rendered = String::new();
    for line in prose.lines().filter(|line| !line.trim().is_empty()) {
        rendered.push_str("/// ");
        rendered.push_str(line.trim());
        rendered.push('\n');
    }
    rendered
}

fn step_index(document: &Document, name: &str) -> EditResult<usize> {
    document
        .steps
        .iter()
        .position(|step| step.name == name)
        .ok_or_else(|| refusal(RefusalCode::UnknownStep, "step does not exist"))
}

fn falls_through(step: &Step) -> bool {
    step.outcomes.is_empty()
        && !matches!(step.body.last(), Some(Statement::Route(_)))
        && !matches!(step.body.last(), Some(Statement::Pipe(pipe)) if matches!(pipe.end, PipeEnd::Route(_)))
}

fn refusal(code: RefusalCode, message: impl Into<String>) -> EditRefusal {
    EditRefusal {
        code,
        message: message.into(),
    }
}

fn refused(code: RefusalCode, message: impl Into<String>) -> EditResponse {
    EditResponse::from_refusal(refusal(code, message))
}

impl EditResponse {
    fn from_refusal(refusal: EditRefusal) -> Self {
        Self {
            ok: false,
            source: None,
            diagnostics: Vec::new(),
            refusal: Some(refusal),
            rename: None,
        }
    }
}
