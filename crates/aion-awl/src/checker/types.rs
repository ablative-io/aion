#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Ty {
    Bool,
    Int,
    Float,
    String,
    Nil,
    Dir,
    Duration,
    List(Box<Ty>),
    Option(Box<Ty>),
    Record(String),
    OpaqueChild,
    Unknown,
}

impl Ty {
    pub(super) fn is_primitive_comparable(&self) -> bool {
        matches!(
            self,
            Self::Bool | Self::Int | Self::Float | Self::String | Self::Duration
        )
    }

    pub(super) fn display(&self) -> String {
        match self {
            Self::Bool => "Bool".to_owned(),
            Self::Int => "Int".to_owned(),
            Self::Float => "Float".to_owned(),
            Self::String => "String".to_owned(),
            Self::Nil => "Nil".to_owned(),
            Self::Dir => "Dir".to_owned(),
            Self::Duration => "Duration".to_owned(),
            Self::List(inner) => format!("List({})", inner.display()),
            Self::Option(inner) => format!("Option({})", inner.display()),
            Self::Record(name) => name.clone(),
            Self::OpaqueChild => "untyped child result".to_owned(),
            Self::Unknown => "<unknown>".to_owned(),
        }
    }
}

#[derive(Debug)]
pub(super) struct ActionSig {
    pub(super) params: Vec<(String, Ty)>,
    pub(super) returns: Ty,
}
