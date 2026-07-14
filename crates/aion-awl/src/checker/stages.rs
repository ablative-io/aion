//! Pipe-chain typing: `|>` stages thread a value through one-argument
//! actions, `.field` projections, and the fixed combinator vocabulary
//! (`filter`/`map`/`sort`/`count`).

use std::rc::Rc;

use crate::ast::{CombinatorCall, CombinatorKind, Expr, PipeEnd, PipeStage, PipeStmt};
use crate::semantic::DeclarationKind;
use crate::spanned::Spanned;

use super::collections::check_element_predicate;
use super::exprs::{View, field_access, type_of};
use super::outcomes::{Env, check_route};
use super::types::{Ty, assignable, resolve};
use super::walk::{Scope, Walker, insert_binding};

/// Walk one pipe statement: type the head, thread it through every stage,
/// then bind or route the final value.
pub(super) fn walk_pipe(
    w: &mut Walker<'_, '_>,
    scope: &mut Scope,
    pipe: &PipeStmt,
    env: &Env<'_>,
) -> Option<Ty> {
    let view = View {
        vars: scope,
        narrow: None,
        accessor: None,
    };
    let mut ty = type_of(w, view, &pipe.head);
    for stage in &pipe.stages {
        ty = pipe_stage(w, view, ty, stage);
    }
    match &pipe.end {
        PipeEnd::Bind(bind) => {
            insert_binding(w, scope, &bind.name, ty.clone(), bind.span);
            Some(ty)
        }
        PipeEnd::Route(target) => {
            let view = View {
                vars: scope,
                narrow: None,
                accessor: None,
            };
            check_route(w, view, target, env, Some(ty));
            None
        }
    }
}

fn pipe_stage(w: &mut Walker<'_, '_>, view: View<'_>, incoming: Ty, stage: &PipeStage) -> Ty {
    match stage {
        PipeStage::Action { span, name } => {
            let Some(callable) = w.ctx.callable(name).cloned() else {
                w.err(
                    *span,
                    format!("no action or child named `{name}` is declared"),
                );
                return Ty::Unknown;
            };
            if w.emit {
                let kind = if w.ctx.actions.contains_key(name) {
                    DeclarationKind::Action
                } else {
                    DeclarationKind::Child
                };
                w.ctx.semantic.reference(*span, kind, name);
                w.ctx.semantic.ty(*span, &callable.returns.to_string());
            }
            if callable.params.len() != 1 {
                w.err(
                    *span,
                    format!(
                        "`{name}` takes {} arguments and cannot be a pipe stage — a \
                         stage receives exactly one value",
                        callable.params.len()
                    ),
                );
                return callable.returns;
            }
            let param = &callable.params[0];
            if !assignable(&incoming, &param.ty, &w.ctx.types) {
                w.err(
                    *span,
                    format!("pipe stage `{name}` expects {}, found {incoming}", param.ty),
                );
            }
            callable.returns
        }
        PipeStage::Field { span, name } => field_access(w, &incoming, None, name, *span),
        PipeStage::Combinator(combinator) => combinator_stage(w, view, incoming, combinator),
    }
}

fn combinator_stage(
    w: &mut Walker<'_, '_>,
    view: View<'_>,
    incoming: Ty,
    combinator: &CombinatorCall,
) -> Ty {
    let element = match resolve(&incoming, &w.ctx.types) {
        Ty::List(inner) => (*inner).clone(),
        Ty::Unknown => Ty::Unknown,
        other => {
            let noun = combinator_name(combinator.kind);
            w.err(
                combinator.span,
                format!("`{noun}` needs a list to work over, found {other}"),
            );
            Ty::Unknown
        }
    };
    match combinator.kind {
        CombinatorKind::Count => {
            if combinator.arg.is_some() {
                w.err(combinator.span, "`count` takes no argument");
            }
            Ty::Int
        }
        CombinatorKind::Filter => {
            if let Some(field_ty) = accessor_field(w, combinator, &element) {
                let resolved = resolve(&field_ty, &w.ctx.types);
                if !matches!(resolved, Ty::Bool | Ty::Unknown)
                    && let Some(Expr::Accessor { span, name }) = combinator.arg.as_ref()
                {
                    w.err(
                        *span,
                        format!(
                            "`filter` keeps items whose accessor is true — `.{name}` \
                             selects {field_ty}, not Bool"
                        ),
                    );
                }
            }
            incoming
        }
        CombinatorKind::Sort => {
            accessor_field(w, combinator, &element);
            incoming
        }
        CombinatorKind::Map => match accessor_field(w, combinator, &element) {
            Some(field_ty) => Ty::List(Rc::new(field_ty)),
            None => Ty::List(Rc::new(Ty::Unknown)),
        },
        CombinatorKind::Any | CombinatorKind::All => {
            let noun = combinator_name(combinator.kind);
            match combinator.arg.as_ref() {
                Some(predicate) => check_element_predicate(w, view, predicate, &element, noun),
                None => w.err(
                    combinator.span,
                    format!("`{noun}` needs a Bool predicate argument"),
                ),
            }
            Ty::Bool
        }
    }
}

const fn combinator_name(kind: CombinatorKind) -> &'static str {
    match kind {
        CombinatorKind::Filter => "filter",
        CombinatorKind::Map => "map",
        CombinatorKind::Sort => "sort",
        CombinatorKind::Count => "count",
        CombinatorKind::Any => "any",
        CombinatorKind::All => "all",
    }
}

/// Resolve a combinator's `.field` accessor argument against the element
/// type, reporting a missing or non-accessor argument.
fn accessor_field(w: &mut Walker<'_, '_>, combinator: &CombinatorCall, element: &Ty) -> Option<Ty> {
    let noun = combinator_name(combinator.kind);
    match combinator.arg.as_ref() {
        Some(Expr::Accessor { span, name }) => Some(field_access(w, element, None, name, *span)),
        Some(other) => {
            w.err(
                other.span(),
                format!("`{noun}` takes a `.field` accessor argument"),
            );
            None
        }
        None => {
            w.err(
                combinator.span,
                format!("`{noun}` needs a `.field` accessor argument"),
            );
            None
        }
    }
}
