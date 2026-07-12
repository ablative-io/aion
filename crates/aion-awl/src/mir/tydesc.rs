//! The total type-descriptor set (AWL-BC-IR.md §5 / Appendix A).
//!
//! `TyDesc` is the sidecar-projection source (S2): every arm maps to a
//! `gleam_types::TypeDescriptor` with no erasure (X1 rejected). Function
//! signatures carry `TyDesc`s; `verify` cross-checks op result types against
//! the rev-2 `TypeEnv` (S1), so no per-edge type ids are carried (X3).

/// The five builtin leaf shapes, shared by codec refs and descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Leaf {
    Bool,
    Int,
    Float,
    Str,
    Nil,
}

impl Leaf {
    /// The `aion/awl/codec` stem (`bool`, `int`, `float`, `string`, `nil`).
    pub(crate) fn stem(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::Str => "string",
            Self::Nil => "nil",
        }
    }
}

/// A total, closed type descriptor. Every arm has a `gleam_types` projection
/// (`crate::mir::sidecar`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TyDesc {
    Bool,
    Int,
    Float,
    String,
    Nil,
    List(Box<TyDesc>),
    Option(Box<TyDesc>),
    Result(Box<TyDesc>, Box<TyDesc>),
    Tuple(Vec<TyDesc>),
    /// A named record/enum/union (module string per S10/IR-23), or an SDK
    /// nominal type carrying its own module and type parameters.
    Custom {
        module: String,
        name: String,
        params: Vec<TyDesc>,
    },
    Fn(Vec<TyDesc>, Box<TyDesc>),
    Dynamic,
    Json,
    AwlError,
    Decoder(Box<TyDesc>),
    Codec(Box<TyDesc>),
    Activity(Box<TyDesc>, Box<TyDesc>),
    SignalRef(Box<TyDesc>),
    WorkflowDefinition(Box<TyDesc>, Box<TyDesc>, Box<TyDesc>),
    Duration,
    /// Empty-list provenance; projects as `List(Nil)` (§5). The reference's
    /// `gleam_type` renders bare `Nil` (`types.rs:116`); the sidecar keeps the
    /// list shape (the judge-adjudicated more-faithful projection).
    Unknown,
}
