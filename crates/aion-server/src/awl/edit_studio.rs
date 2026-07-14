use std::collections::BTreeSet;

use aion_awl::{
    Document, Expr, ForkHeader, Guard, PipeEnd, PipeStage, Statement, Step, TypeBody, TypeRef,
    semantic,
};

use super::edit::{parse_fragment, refusal};
use super::edit_types::{
    ActionDefinition, ActionParameter, EditResult, RefusalCode, RenameMapping, TypeField,
};

const BUILTIN_TYPES: [&str; 6] = ["Bool", "Int", "Float", "String", "Nil", "Dir"];

type StudioResult = EditResult<Option<RenameMapping>>;

pub(super) fn add_type(document: &mut Document, name: &str, fields: &[TypeField]) -> StudioResult {
    ensure_type_name_available(document, name)?;
    let field_source = fields
        .iter()
        .map(|field| format!("{}: {}", field.name, field.ty))
        .collect::<Vec<_>>()
        .join(", ");
    let mut fragment = parse_fragment(&format!("type {name} {{ {field_source} }}\n"))?;
    let declaration = fragment.types.pop().ok_or_else(|| {
        refusal(
            RefusalCode::InvalidOperation,
            "record type could not be constructed",
        )
    })?;
    let TypeBody::Record { fields: parsed } = &declaration.body else {
        return Err(refusal(
            RefusalCode::InvalidOperation,
            "record type did not parse as a record",
        ));
    };
    ensure_unique(fields.iter().map(|field| field.name.as_str()), "field")?;
    for field in parsed {
        validate_type_ref(document, &field.ty)?;
    }
    document.types.push(declaration);
    Ok(None)
}

pub(super) fn add_type_field(
    document: &mut Document,
    type_name: &str,
    name: &str,
    field_type: &str,
) -> StudioResult {
    let mut fragment = parse_fragment(&format!("type GestureType {{ {name}: {field_type} }}\n"))?;
    let field = fragment
        .types
        .pop()
        .and_then(|declaration| match declaration.body {
            TypeBody::Record { mut fields } => fields.pop(),
            _ => None,
        })
        .ok_or_else(|| {
            refusal(
                RefusalCode::InvalidOperation,
                "record field could not be constructed",
            )
        })?;
    validate_type_ref(document, &field.ty)?;
    let declaration = document
        .types
        .iter_mut()
        .find(|declaration| declaration.name == type_name)
        .ok_or_else(|| refusal(RefusalCode::UnknownType, "type does not exist"))?;
    let TypeBody::Record { fields } = &mut declaration.body else {
        return Err(refusal(
            RefusalCode::InvalidOperation,
            "fields can only be added to record types",
        ));
    };
    if fields.iter().any(|field| field.name == name) {
        return Err(refusal(
            RefusalCode::NameCollision,
            "field name already exists",
        ));
    }
    fields.push(field);
    Ok(None)
}

pub(super) fn remove_type_field(
    document: &mut Document,
    type_name: &str,
    name: &str,
) -> StudioResult {
    let declaration = document
        .types
        .iter()
        .find(|declaration| declaration.name == type_name)
        .ok_or_else(|| refusal(RefusalCode::UnknownType, "type does not exist"))?;
    let TypeBody::Record { fields } = &declaration.body else {
        return Err(refusal(
            RefusalCode::InvalidOperation,
            "fields can only be removed from record types",
        ));
    };
    if !fields.iter().any(|field| field.name == name) {
        return Err(refusal(RefusalCode::UnknownField, "field does not exist"));
    }
    if document
        .steps
        .iter()
        .any(|step| step_uses_field(step, name))
    {
        return Err(refusal(
            RefusalCode::TypeInUse,
            "field is referenced by the workflow",
        ));
    }
    let declaration = document
        .types
        .iter_mut()
        .find(|declaration| declaration.name == type_name)
        .ok_or_else(|| refusal(RefusalCode::UnknownType, "type does not exist"))?;
    let TypeBody::Record { fields } = &mut declaration.body else {
        return Err(refusal(
            RefusalCode::InvalidOperation,
            "fields can only be removed from record types",
        ));
    };
    fields.retain(|field| field.name != name);
    Ok(None)
}

pub(super) fn add_enum_type(
    document: &mut Document,
    name: &str,
    variants: &[String],
) -> StudioResult {
    ensure_type_name_available(document, name)?;
    if variants.is_empty() {
        return Err(refusal(
            RefusalCode::InvalidOperation,
            "enum types require at least one variant",
        ));
    }
    ensure_unique(variants.iter().map(String::as_str), "variant")?;
    let mut fragment = parse_fragment(&format!("type {name} = {}\n", variants.join(" | ")))?;
    let declaration = fragment.types.pop().ok_or_else(|| {
        refusal(
            RefusalCode::InvalidOperation,
            "enum type could not be constructed",
        )
    })?;
    document.types.push(declaration);
    Ok(None)
}

pub(super) fn add_worker(
    document: &mut Document,
    name: &str,
    action: &ActionDefinition,
) -> StudioResult {
    if document.workers.iter().any(|worker| worker.name == name) {
        return Err(refusal(
            RefusalCode::NameCollision,
            "worker name already exists",
        ));
    }
    validate_action_signature(document, &action.params, &action.return_type)?;
    let parameters = render_parameters(&action.params);
    let source = format!(
        "worker {name}\n  action {}({parameters}) -> {}\n",
        action.name, action.return_type
    );
    let mut fragment = parse_fragment(&source)?;
    let worker = fragment.workers.pop().ok_or_else(|| {
        refusal(
            RefusalCode::InvalidOperation,
            "worker could not be constructed",
        )
    })?;
    document.workers.push(worker);
    Ok(None)
}

pub(super) fn remove_worker(document: &mut Document, name: &str) -> StudioResult {
    let index = document
        .workers
        .iter()
        .position(|worker| worker.name == name)
        .ok_or_else(|| refusal(RefusalCode::UnknownWorker, "worker does not exist"))?;
    let action_spans = document.workers[index]
        .actions
        .iter()
        .map(|action| action.name_span);
    if declaration_referenced(document, action_spans) {
        return Err(refusal(
            RefusalCode::ActionInUse,
            "worker has an action referenced by a workflow step",
        ));
    }
    document.workers.remove(index);
    Ok(None)
}

pub(super) fn remove_action(
    document: &mut Document,
    worker_name: &str,
    name: &str,
) -> StudioResult {
    let worker = document
        .workers
        .iter()
        .find(|worker| worker.name == worker_name)
        .ok_or_else(|| refusal(RefusalCode::UnknownWorker, "worker does not exist"))?;
    let action = worker
        .actions
        .iter()
        .find(|action| action.name == name)
        .ok_or_else(|| refusal(RefusalCode::UnknownAction, "action does not exist"))?;
    if worker.actions.len() == 1 {
        return Err(refusal(
            RefusalCode::LastAction,
            "a worker requires an action; remove the worker instead",
        ));
    }
    if declaration_referenced(document, [action.name_span]) {
        return Err(refusal(
            RefusalCode::ActionInUse,
            "action is referenced by a workflow step",
        ));
    }
    let worker = document
        .workers
        .iter_mut()
        .find(|worker| worker.name == worker_name)
        .ok_or_else(|| refusal(RefusalCode::UnknownWorker, "worker does not exist"))?;
    worker.actions.retain(|action| action.name != name);
    Ok(None)
}

pub(super) fn validate_action_signature(
    document: &Document,
    params: &[ActionParameter],
    return_type: &str,
) -> EditResult<()> {
    ensure_unique(
        params.iter().map(|parameter| parameter.name.as_str()),
        "parameter",
    )?;
    let source = format!(
        "worker gesture_worker\n  action gesture_action({}) -> {return_type}\n",
        render_parameters(params)
    );
    let mut fragment = parse_fragment(&source)?;
    let action = fragment
        .workers
        .pop()
        .and_then(|mut worker| worker.actions.pop())
        .ok_or_else(|| {
            refusal(
                RefusalCode::InvalidOperation,
                "action signature could not be constructed",
            )
        })?;
    for parameter in &action.params {
        validate_type_ref(document, &parameter.ty)?;
    }
    validate_type_ref(document, &action.returns)
}

fn render_parameters(params: &[ActionParameter]) -> String {
    params
        .iter()
        .map(|parameter| format!("{}: {}", parameter.name, parameter.ty))
        .collect::<Vec<_>>()
        .join(", ")
}

fn ensure_type_name_available(document: &Document, name: &str) -> EditResult<()> {
    if BUILTIN_TYPES.contains(&name) || document.types.iter().any(|item| item.name == name) {
        return Err(refusal(
            RefusalCode::NameCollision,
            "type name already exists",
        ));
    }
    Ok(())
}

fn validate_type_ref(document: &Document, ty: &TypeRef) -> EditResult<()> {
    match ty {
        TypeRef::Named { name, .. } => {
            if BUILTIN_TYPES.contains(&name.as_str())
                || document.types.iter().any(|item| item.name == *name)
            {
                Ok(())
            } else {
                Err(refusal(
                    RefusalCode::UnknownType,
                    format!("type `{name}` does not exist"),
                ))
            }
        }
        TypeRef::List { inner, .. } | TypeRef::Optional { inner, .. } => {
            validate_type_ref(document, inner)
        }
    }
}

fn declaration_referenced(
    document: &Document,
    spans: impl IntoIterator<Item = aion_awl::Span>,
) -> bool {
    let targets: BTreeSet<_> = spans
        .into_iter()
        .map(|span| (span.start, span.end))
        .collect();
    semantic::analyze(document).iter().any(|info| {
        info.declaration.as_ref().is_some_and(|declaration| {
            let target = (declaration.span.start, declaration.span.end);
            targets.contains(&target) && (info.span.start, info.span.end) != target
        })
    })
}

fn step_uses_field(step: &Step, name: &str) -> bool {
    step.body
        .iter()
        .any(|statement| statement_uses_field(statement, name))
        || step.on_failure.as_ref().is_some_and(|failure| {
            failure
                .body
                .iter()
                .any(|statement| statement_uses_field(statement, name))
        })
        || step.outcomes.iter().any(|outcome| {
            matches!(&outcome.guard, Guard::When { expr, .. } if expr_uses_field(expr, name))
                || route_uses_field(outcome.route.payload.as_deref(), name)
        })
}

fn statement_uses_field(statement: &Statement, name: &str) -> bool {
    match statement {
        Statement::Call(call) => args_use_field(&call.call.args, name),
        Statement::Spawn(spawn) => args_use_field(&spawn.call.args, name),
        Statement::Pipe(pipe) => {
            expr_uses_field(&pipe.head, name)
                || pipe.stages.iter().any(|stage| match stage {
                    PipeStage::Field { name: field, .. } => field == name,
                    PipeStage::Combinator(combinator) => combinator
                        .arg
                        .as_ref()
                        .is_some_and(|expr| expr_uses_field(expr, name)),
                    PipeStage::Action { .. } => false,
                })
                || matches!(&pipe.end, PipeEnd::Route(target) if route_uses_field(target.payload.as_deref(), name))
        }
        Statement::Fork(fork) => {
            matches!(&fork.header, ForkHeader::Collection { collection, .. } if expr_uses_field(collection, name))
                || fork
                    .body
                    .iter()
                    .any(|statement| statement_uses_field(statement, name))
        }
        Statement::Loop(loop_statement) => {
            expr_uses_field(&loop_statement.seed, name)
                || loop_statement
                    .body
                    .iter()
                    .any(|statement| statement_uses_field(statement, name))
                || loop_statement
                    .until
                    .as_ref()
                    .is_some_and(|tail| expr_uses_field(&tail.expr, name))
                || loop_statement
                    .max
                    .as_ref()
                    .is_some_and(|tail| expr_uses_field(&tail.expr, name))
        }
        Statement::Route(route) => route_uses_field(route.target.payload.as_deref(), name),
        Statement::SubStep(step) => step_uses_field(step, name),
        Statement::Wait(_) | Statement::Sleep(_) => false,
    }
}

fn route_uses_field(payload: Option<&[aion_awl::Arg]>, name: &str) -> bool {
    payload.is_some_and(|arguments| args_use_field(arguments, name))
}

fn args_use_field(arguments: &[aion_awl::Arg], name: &str) -> bool {
    arguments
        .iter()
        .any(|argument| expr_uses_field(&argument.value, name))
}

fn expr_uses_field(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::List { items, .. } => items.iter().any(|item| expr_uses_field(item, name)),
        Expr::Record { args, .. } => args
            .iter()
            .any(|argument| argument.name == name || expr_uses_field(&argument.value, name)),
        Expr::Field { base, .. } if matches!(base.as_ref(), Expr::Workflow { .. }) => false,
        Expr::Field {
            base, name: field, ..
        } => field == name || expr_uses_field(base, name),
        Expr::Index { base, .. } => expr_uses_field(base, name),
        Expr::Accessor { name: field, .. } => field == name,
        Expr::Not { expr, .. } => expr_uses_field(expr, name),
        Expr::Binary { left, right, .. } => {
            expr_uses_field(left, name) || expr_uses_field(right, name)
        }
        Expr::Predicate { subject, .. } => expr_uses_field(subject, name),
        Expr::CollectionPredicate {
            collection,
            predicate,
            ..
        } => expr_uses_field(collection, name) || expr_uses_field(predicate, name),
        Expr::String { .. }
        | Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Duration(_)
        | Expr::Ref { .. }
        | Expr::Workflow { .. }
        | Expr::Variant { .. } => false,
    }
}

fn ensure_unique<'a>(values: impl IntoIterator<Item = &'a str>, kind: &str) -> EditResult<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(refusal(
                RefusalCode::NameCollision,
                format!("{kind} name is duplicated"),
            ));
        }
    }
    Ok(())
}
