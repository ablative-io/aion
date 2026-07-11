//! The checker's semantic types: builtins, lists, optionals, record and enum
//! shapes, and the structural-compatibility rules that let a schema-projected
//! record satisfy a declared shorthand record with the same shape.

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use crate::Span;

/// One field of a record shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FieldTy {
    /// Field name.
    pub(super) name: String,
    /// Field type; optionality is carried by `Ty::Optional`.
    pub(super) ty: Ty,
    /// Source declaration for authored fields; projected fields have none.
    pub(super) declaration: Option<Span>,
}

/// A record shape, named when it came from a declaration or a `$defs` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RecordTy {
    /// Declared or `$defs` name, `None` for anonymous nested objects.
    pub(super) name: Option<String>,
    /// Fields in declaration order.
    pub(super) fields: Vec<FieldTy>,
}

impl RecordTy {
    /// Look up a field by name.
    pub(super) fn field(&self, name: &str) -> Option<&FieldTy> {
        self.fields.iter().find(|field| field.name == name)
    }
}

/// A payload-less enum shape (declared) or a string enum (projected).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EnumTy {
    /// Declared or `$defs` name, `None` for anonymous projections.
    pub(super) name: Option<String>,
    /// Variant names in declaration order.
    pub(super) variants: Vec<String>,
}

/// A semantic type in the checker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Ty {
    /// Builtin `Bool`.
    Bool,
    /// Builtin `Int`.
    Int,
    /// Builtin `Float`.
    Float,
    /// Builtin `String`.
    Str,
    /// Builtin `Nil`.
    Nil,
    /// Builtin `Dir` (content-addressed snapshot handle).
    Dir,
    /// A duration literal.
    Duration,
    /// List `[T]`.
    List(Rc<Ty>),
    /// Optional `T?` — the value may be absent, never null.
    Optional(Rc<Ty>),
    /// A reference to a declared type, resolved lazily through the table.
    Named(String),
    /// A structural record shape (schema projections, `$defs` entries).
    Record(Rc<RecordTy>),
    /// An enum shape.
    Enum(Rc<EnumTy>),
    /// Error-recovery type; compatible with everything, silences cascades.
    Unknown,
}

/// The declared-type table: name → definition shape.
pub(super) type TypeTable = BTreeMap<String, Ty>;

impl Ty {
    /// Wrap in `Optional` unless already optional or unknown.
    pub(super) fn optional(self) -> Self {
        match self {
            Self::Optional(_) | Self::Unknown => self,
            other => Self::Optional(Rc::new(other)),
        }
    }
}

/// Follow `Named` references through the table to a structural definition.
///
/// Returns `Unknown` for names missing from the table (already reported at
/// declaration time).
pub(super) fn resolve(ty: &Ty, table: &TypeTable) -> Ty {
    let mut current = ty.clone();
    for _ in 0..16 {
        match current {
            Ty::Named(ref name) => match table.get(name) {
                Some(definition) => current = definition.clone(),
                None => return Ty::Unknown,
            },
            other => return other,
        }
    }
    Ty::Unknown
}

/// Whether a value of type `actual` can be used where `expected` is declared.
///
/// Compatibility is structural: a schema-projected record satisfies a
/// declared record with the same field names, types, and optionality. A
/// present `T` satisfies an expected `T?`; the reverse never holds.
pub(super) fn assignable(actual: &Ty, expected: &Ty, table: &TypeTable) -> bool {
    matches_ty(actual, expected, table, &mut Vec::new(), true)
}

/// Structural type equality with `Unknown` treated as compatible.
pub(super) fn same_ty(a: &Ty, b: &Ty, table: &TypeTable) -> bool {
    matches_ty(a, b, table, &mut Vec::new(), false)
}

/// Named-pair comparisons currently in progress, for coinductive
/// termination on recursive types: a pair already under comparison is
/// assumed compatible (its structure is being proven right now), so
/// recursion terminates without ever accepting a genuine mismatch.
type Comparing = Vec<(String, String)>;

fn matches_ty(
    actual: &Ty,
    expected: &Ty,
    table: &TypeTable,
    seen: &mut Comparing,
    widen: bool,
) -> bool {
    if matches!(actual, Ty::Unknown) || matches!(expected, Ty::Unknown) {
        return true;
    }
    if let (Ty::Named(a), Ty::Named(b)) = (actual, expected) {
        if a == b {
            return true;
        }
        let key = (a.clone(), b.clone());
        if seen.contains(&key) {
            return true;
        }
        seen.push(key);
        let compatible = matches_resolved(actual, expected, table, seen, widen);
        seen.pop();
        return compatible;
    }
    matches_resolved(actual, expected, table, seen, widen)
}

fn matches_resolved(
    actual: &Ty,
    expected: &Ty,
    table: &TypeTable,
    seen: &mut Comparing,
    widen: bool,
) -> bool {
    let actual = resolve(actual, table);
    let expected = resolve(expected, table);
    if matches!(actual, Ty::Unknown) || matches!(expected, Ty::Unknown) {
        return true;
    }
    match (&actual, &expected) {
        (Ty::Optional(a), Ty::Optional(e)) | (Ty::List(a), Ty::List(e)) => {
            matches_ty(a, e, table, seen, false)
        }
        (a, Ty::Optional(e)) if widen => matches_ty(a, e, table, seen, false),
        (Ty::Record(a), Ty::Record(e)) => {
            if a.fields.len() != e.fields.len() {
                return false;
            }
            a.fields.iter().all(|field| {
                e.field(&field.name)
                    .is_some_and(|other| matches_ty(&field.ty, &other.ty, table, seen, false))
            })
        }
        (Ty::Enum(a), Ty::Enum(e)) => {
            let mut ours: Vec<&String> = a.variants.iter().collect();
            let mut theirs: Vec<&String> = e.variants.iter().collect();
            ours.sort();
            theirs.sort();
            ours == theirs
        }
        (Ty::Bool, Ty::Bool)
        | (Ty::Int, Ty::Int)
        | (Ty::Float, Ty::Float)
        | (Ty::Str, Ty::Str)
        | (Ty::Nil, Ty::Nil)
        | (Ty::Dir, Ty::Dir)
        | (Ty::Duration, Ty::Duration) => true,
        _ => false,
    }
}

/// Whether `==` / `!=` may compare the two types.
///
/// Primitives compare with themselves; enums compare with structurally equal
/// enums and with string literals (imported string enums).
pub(super) fn equality_comparable(a: &Ty, b: &Ty, table: &TypeTable) -> bool {
    let left = resolve(a, table);
    let right = resolve(b, table);
    if matches!(left, Ty::Unknown) || matches!(right, Ty::Unknown) {
        return true;
    }
    match (&left, &right) {
        (Ty::Enum(_), Ty::Enum(_)) => same_ty(&left, &right, table),
        _ => {
            matches!(
                (&left, &right),
                (Ty::Enum(_), Ty::Str)
                    | (Ty::Str, Ty::Enum(_) | Ty::Str)
                    | (Ty::Bool, Ty::Bool)
                    | (Ty::Int, Ty::Int)
                    | (Ty::Float, Ty::Float)
                    | (Ty::Dir, Ty::Dir)
            )
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool => write!(f, "Bool"),
            Self::Int => write!(f, "Int"),
            Self::Float => write!(f, "Float"),
            Self::Str => write!(f, "String"),
            Self::Nil => write!(f, "Nil"),
            Self::Dir => write!(f, "Dir"),
            Self::Duration => write!(f, "a duration"),
            Self::List(inner) => write!(f, "[{inner}]"),
            Self::Optional(inner) => write!(f, "{inner}?"),
            Self::Named(name) => write!(f, "{name}"),
            Self::Record(record) => match &record.name {
                Some(name) => write!(f, "{name}"),
                None => write!(f, "an object"),
            },
            Self::Enum(spec) => match &spec.name {
                Some(name) => write!(f, "{name}"),
                None => write!(f, "an enum"),
            },
            Self::Unknown => write!(f, "an unknown type"),
        }
    }
}
