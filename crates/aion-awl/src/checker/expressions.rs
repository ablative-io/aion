use std::collections::HashSet;

use crate::{BinaryOp, Expr, RecordField, Span, Spanned, TypeRef};

use super::{context::Ctx, types::Ty};

impl Ctx<'_> {
    pub(super) fn expr_ty(&mut self, expr: &Expr) -> Ty {
        match expr {
            Expr::String { .. } => Ty::String,
            Expr::Int { .. } => Ty::Int,
            Expr::Float { .. } => Ty::Float,
            Expr::Bool { .. } => Ty::Bool,
            Expr::Duration(_) => Ty::Duration,
            Expr::List { span, items } => self.list_ty(*span, items),
            Expr::Ref { span, name } => {
                self.check_value_name(name, *span, "reference");
                if let Some(ty) = self.bindings.get(name) {
                    ty.clone()
                } else {
                    self.error(*span, format!("unresolved reference `{name}`"));
                    Ty::Unknown
                }
            }
            Expr::Field { span, base, field } => self.field_ty(*span, base, field),
            Expr::Record { span, name, fields } => self.record_ty(*span, name, fields),
            Expr::Not { span, expr } => {
                let found = self.expr_ty(expr);
                self.expect_type(*span, &found, &Ty::Bool, "not operand");
                Ty::Bool
            }
            Expr::Binary {
                span,
                left,
                op,
                right,
            } => self.binary_ty(*span, left, *op, right),
        }
    }

    fn list_ty(&mut self, span: Span, items: &[Expr]) -> Ty {
        let Some((first, rest)) = items.split_first() else {
            self.error(span, "empty list literal has no inferable element type");
            return Ty::Unknown;
        };
        let element = self.expr_ty(first);
        for item in rest {
            let found = self.expr_ty(item);
            self.expect_type(item.span(), &found, &element, "list element");
        }
        Ty::List(Box::new(element))
    }

    fn field_ty(&mut self, span: Span, base: &Expr, field: &str) -> Ty {
        match self.expr_ty(base) {
            Ty::Record(name) => {
                let Some(decl) = self.types.get(name.as_str()) else {
                    self.error(span, format!("unknown type `{name}`"));
                    return Ty::Unknown;
                };
                let Some(found) = decl
                    .fields
                    .iter()
                    .find(|decl_field| decl_field.name == field)
                else {
                    self.error(span, format!("type `{name}` has no field `{field}`"));
                    return Ty::Unknown;
                };
                let ty = found.ty.clone();
                Self::resolve_type_ref(&ty)
            }
            Ty::OpaqueChild => {
                self.error(
                    span,
                    "child result is untyped in this revision and cannot be field-accessed",
                );
                Ty::Unknown
            }
            Ty::Unknown => Ty::Unknown,
            found => {
                self.error(
                    span,
                    format!(
                        "field access expected record type, found {}",
                        found.display()
                    ),
                );
                Ty::Unknown
            }
        }
    }

    fn record_ty(&mut self, span: Span, name: &str, fields: &[RecordField]) -> Ty {
        self.check_type_name(name, span);
        let Some(decl) = self.types.get(name) else {
            self.error(span, format!("unknown record type `{name}`"));
            for field in fields {
                self.expr_ty(&field.value);
            }
            return Ty::Unknown;
        };
        let field_refs: Vec<(String, TypeRef)> = decl
            .fields
            .iter()
            .map(|field| (field.name.clone(), field.ty.clone()))
            .collect();
        let decl_fields: Vec<(String, Ty)> = field_refs
            .iter()
            .map(|(field, ty)| (field.clone(), Self::resolve_type_ref(ty)))
            .collect();
        let mut seen = HashSet::new();
        for field in fields {
            self.check_value_name(&field.name, field.span, "record field");
            if !seen.insert(field.name.as_str()) {
                self.error(field.span, format!("duplicate field `{}`", field.name));
                self.expr_ty(&field.value);
                continue;
            }
            let Some((_, expected)) = decl_fields
                .iter()
                .find(|(decl_field, _)| decl_field == &field.name)
            else {
                self.error(
                    field.span,
                    format!("extra field `{}` for record `{name}`", field.name),
                );
                self.expr_ty(&field.value);
                continue;
            };
            let found = self.expr_ty(&field.value);
            self.expect_type(
                field.value.span(),
                &found,
                expected,
                format!("field `{}`", field.name),
            );
        }
        for (decl_field, _) in &decl_fields {
            if !seen.contains(decl_field.as_str()) {
                self.error(
                    span,
                    format!("missing field `{decl_field}` for record `{name}`"),
                );
            }
        }
        Ty::Record(name.to_owned())
    }

    fn binary_ty(&mut self, span: Span, left: &Expr, op: BinaryOp, right: &Expr) -> Ty {
        let left_ty = self.expr_ty(left);
        let right_ty = self.expr_ty(right);
        match op {
            BinaryOp::And | BinaryOp::Or => {
                self.expect_type(left.span(), &left_ty, &Ty::Bool, "left boolean operand");
                self.expect_type(right.span(), &right_ty, &Ty::Bool, "right boolean operand");
                Ty::Bool
            }
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => {
                if left_ty != Ty::Unknown
                    && right_ty != Ty::Unknown
                    && (left_ty != right_ty || !left_ty.is_primitive_comparable())
                {
                    self.error(
                        span,
                        format!(
                            "comparison expected matching primitive operands, found {} and {}",
                            left_ty.display(),
                            right_ty.display()
                        ),
                    );
                }
                Ty::Bool
            }
            BinaryOp::Add => {
                self.expect_type(left.span(), &left_ty, &Ty::String, "left + operand");
                self.expect_type(right.span(), &right_ty, &Ty::String, "right + operand");
                Ty::String
            }
        }
    }
}
