//! Declaration pass: header hygiene, naming conventions, type registration
//! and resolution (all three doors), worker/action and child contracts.

use std::rc::Rc;

use crate::ast::{ParamDecl, TypeBody, TypeDecl, TypeRef};

use super::context::{BUILTIN_TYPES, Callable, Ctx, Param};
use super::project::project_door;
use super::types::{EnumTy, FieldTy, RecordTy, Ty};

/// Whether a name is `snake_case` (`[a-z][a-z0-9_]*`).
pub(super) fn is_snake_case(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether a name is `TitleCase` (`[A-Z][A-Za-z0-9]*`).
fn is_title_case(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_uppercase()) && chars.all(char::is_alphanumeric)
}

/// Run the declaration pass, populating every table in the context.
pub(super) fn run(ctx: &mut Ctx<'_>) {
    let doc = ctx.doc;
    if !is_snake_case(&doc.name) {
        ctx.error(
            doc.name_span,
            format!(
                "workflow name `{}` must be snake_case ([a-z][a-z0-9_]*)",
                doc.name
            ),
        );
    }
    register_type_names(ctx);
    check_header(ctx);
    resolve_type_bodies(ctx);
    check_workers_and_children(ctx);
    check_subflows(ctx);
}

fn register_type_names(ctx: &mut Ctx<'_>) {
    let doc = ctx.doc;
    for decl in &doc.types {
        if BUILTIN_TYPES.contains(&decl.name.as_str()) {
            ctx.error(
                decl.name_span,
                format!("`{}` is a builtin type and cannot be redeclared", decl.name),
            );
            continue;
        }
        if !is_title_case(&decl.name) {
            ctx.error(
                decl.name_span,
                format!(
                    "type name `{}` must be TitleCase ([A-Z][A-Za-z0-9]*)",
                    decl.name
                ),
            );
        }
        if !ctx.type_names.insert(decl.name.clone()) {
            ctx.error(
                decl.name_span,
                format!("duplicate type declaration `{}`", decl.name),
            );
        }
    }
}

fn check_header(ctx: &mut Ctx<'_>) {
    let doc = ctx.doc;
    for input in &doc.inputs {
        if !is_snake_case(&input.name) {
            ctx.error(
                input.name_span,
                format!(
                    "input name `{}` must be snake_case ([a-z][a-z0-9_]*)",
                    input.name
                ),
            );
        }
        let ty = ctx.resolve_type_ref(&input.ty);
        ctx.semantic.ty(input.name_span, &ty.to_string());
        if ctx.inputs.insert(input.name.clone(), ty).is_some() {
            ctx.error(
                input.name_span,
                format!("duplicate input declaration `{}`", input.name),
            );
        }
    }
    for signal in &doc.signals {
        if !is_snake_case(&signal.name) {
            ctx.error(
                signal.name_span,
                format!(
                    "signal name `{}` must be snake_case ([a-z][a-z0-9_]*)",
                    signal.name
                ),
            );
        }
        let ty = ctx.resolve_type_ref(&signal.ty);
        ctx.semantic.ty(signal.name_span, &ty.to_string());
        if ctx.signals.insert(signal.name.clone(), ty).is_some() {
            ctx.error(
                signal.name_span,
                format!("duplicate signal declaration `{}`", signal.name),
            );
        }
    }
    for outcome in &doc.outcomes {
        if !is_snake_case(&outcome.name) {
            ctx.error(
                outcome.name_span,
                format!(
                    "outcome name `{}` must be snake_case ([a-z][a-z0-9_]*)",
                    outcome.name
                ),
            );
        }
        let ty = ctx.resolve_type_ref(&outcome.ty);
        ctx.semantic.ty(outcome.name_span, &ty.to_string());
        if ctx.outcome_types.insert(outcome.name.clone(), ty).is_some() {
            ctx.error(
                outcome.name_span,
                format!("duplicate outcome declaration `{}`", outcome.name),
            );
        }
    }
}

fn resolve_type_bodies(ctx: &mut Ctx<'_>) {
    let doc = ctx.doc;
    for decl in &doc.types {
        let definition = match &decl.body {
            TypeBody::Record { .. } => resolve_record(ctx, decl),
            TypeBody::Enum { .. } => resolve_enum(ctx, decl),
            TypeBody::SchemaInline { .. } | TypeBody::SchemaImport { .. } => {
                project_door(ctx, decl)
            }
        };
        ctx.semantic
            .ty(decl.name_span, &Ty::Named(decl.name.clone()).to_string());
        // The first declaration wins under duplicates; the duplicate itself
        // is already reported at registration.
        ctx.types.entry(decl.name.clone()).or_insert(definition);
    }
}

fn resolve_record(ctx: &mut Ctx<'_>, decl: &TypeDecl) -> Ty {
    let TypeBody::Record { fields } = &decl.body else {
        return Ty::Unknown;
    };
    let mut resolved: Vec<FieldTy> = Vec::new();
    for field in fields {
        if !is_snake_case(&field.name) {
            ctx.error(
                field.name_span,
                format!(
                    "field name `{}` must be snake_case ([a-z][a-z0-9_]*)",
                    field.name
                ),
            );
        }
        let ty = ctx.resolve_type_ref(&field.ty);
        ctx.semantic.ty(field.name_span, &ty.to_string());
        if resolved.iter().any(|existing| existing.name == field.name) {
            ctx.error(
                field.name_span,
                format!("duplicate field `{}` in type `{}`", field.name, decl.name),
            );
            continue;
        }
        resolved.push(FieldTy {
            name: field.name.clone(),
            ty,
            declaration: Some(field.name_span),
        });
    }
    Ty::Record(Rc::new(RecordTy {
        name: Some(decl.name.clone()),
        fields: resolved,
    }))
}

fn resolve_enum(ctx: &mut Ctx<'_>, decl: &TypeDecl) -> Ty {
    let TypeBody::Enum { variants } = &decl.body else {
        return Ty::Unknown;
    };
    let mut names: Vec<String> = Vec::new();
    for variant in variants {
        if !is_title_case(&variant.name) {
            ctx.error(
                variant.span,
                format!(
                    "enum variant `{}` must be TitleCase ([A-Z][A-Za-z0-9]*)",
                    variant.name
                ),
            );
        }
        if names.contains(&variant.name) {
            ctx.error(
                variant.span,
                format!(
                    "duplicate enum variant `{}` in `{}`",
                    variant.name, decl.name
                ),
            );
            continue;
        }
        names.push(variant.name.clone());
    }
    Ty::Enum(Rc::new(EnumTy {
        name: Some(decl.name.clone()),
        variants: names,
    }))
}

fn check_workers_and_children(ctx: &mut Ctx<'_>) {
    let doc = ctx.doc;
    for worker in &doc.workers {
        for action in &worker.actions {
            let callable = resolve_callable(ctx, &action.params, &action.returns);
            ctx.semantic
                .ty(action.name_span, &callable.returns.to_string());
            if ctx.children.contains_key(&action.name) {
                ctx.error(
                    action.name_span,
                    format!(
                        "`{}` is declared as both a child and an action",
                        action.name
                    ),
                );
            }
            if ctx.actions.insert(action.name.clone(), callable).is_some() {
                ctx.error(
                    action.name_span,
                    format!("duplicate action declaration `{}`", action.name),
                );
            }
        }
    }
    for child in &doc.children {
        let callable = resolve_callable(ctx, &child.params, &child.returns);
        ctx.semantic
            .ty(child.name_span, &callable.returns.to_string());
        if ctx.actions.contains_key(&child.name) {
            ctx.error(
                child.name_span,
                format!("`{}` is declared as both an action and a child", child.name),
            );
        }
        if ctx.children.insert(child.name.clone(), callable).is_some() {
            ctx.error(
                child.name_span,
                format!("duplicate child declaration `{}`", child.name),
            );
        }
    }
}

/// Register subflow contracts: parameters (the subflow's inputs) and the
/// single outcome's payload type (what an invocation binds). Subflows share
/// the call namespace with actions and children, so the names must not
/// collide.
fn check_subflows(ctx: &mut Ctx<'_>) {
    let doc = ctx.doc;
    for subflow in &doc.subflows {
        if !is_snake_case(&subflow.name) {
            ctx.error(
                subflow.name_span,
                format!(
                    "subflow name `{}` must be snake_case ([a-z][a-z0-9_]*)",
                    subflow.name
                ),
            );
        }
        if !is_snake_case(&subflow.outcome.name) {
            ctx.error(
                subflow.outcome.name_span,
                format!(
                    "outcome name `{}` must be snake_case ([a-z][a-z0-9_]*)",
                    subflow.outcome.name
                ),
            );
        }
        let callable = resolve_callable(ctx, &subflow.params, &subflow.outcome.ty);
        ctx.semantic
            .ty(subflow.name_span, &callable.returns.to_string());
        ctx.semantic
            .ty(subflow.outcome.name_span, &callable.returns.to_string());
        if ctx.actions.contains_key(&subflow.name) || ctx.children.contains_key(&subflow.name) {
            ctx.error(
                subflow.name_span,
                format!(
                    "`{}` is already declared as an action or child — subflows share the \
                     call namespace",
                    subflow.name
                ),
            );
        }
        if ctx
            .subflows
            .insert(subflow.name.clone(), callable)
            .is_some()
        {
            ctx.error(
                subflow.name_span,
                format!("duplicate subflow declaration `{}`", subflow.name),
            );
        }
    }
}

fn resolve_callable(ctx: &mut Ctx<'_>, params: &[ParamDecl], returns: &TypeRef) -> Callable {
    let mut resolved: Vec<Param> = Vec::new();
    for param in params {
        let ty = ctx.resolve_type_ref(&param.ty);
        ctx.semantic.ty(param.name_span, &ty.to_string());
        if resolved.iter().any(|existing| existing.name == param.name) {
            ctx.error(
                param.name_span,
                format!("duplicate parameter `{}`", param.name),
            );
            continue;
        }
        resolved.push(Param {
            name: param.name.clone(),
            ty,
        });
    }
    Callable {
        params: resolved,
        returns: ctx.resolve_type_ref(returns),
    }
}
