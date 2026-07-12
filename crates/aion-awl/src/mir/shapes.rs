//! Module-level type shapes and literals (AWL-BC-IR.md Appendix A).
//!
//! `TypeShape` is the registry of declared/projected records, enums, and the
//! outcome union; `WireDesc` is `AWL-BC-CODEC-DESIGN.md` §2 `Desc` as pure
//! lowering-time data (decision 9) — codec-template parameters, never a
//! descriptor engine. `MirLiteral` carries only beamr `Literal` shapes.

use super::ids::AtomRef;

/// `AWL-BC-CODEC-DESIGN.md` §2 `Desc` reused as pure lowering-time data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireDesc {
    Bool,
    Int,
    Float,
    Str,
    Nil,
    List(Box<WireDesc>),
    Nullable(Box<WireDesc>),
    /// Reference into the `TypeShape` registry by name.
    Ref(String),
}

/// One record field's wire shape (`optional` is D4 field optionality).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldShape {
    pub awl_name: String,
    pub desc: WireDesc,
    pub optional: bool,
}

/// One arm of the outcome union: JSON `outcome` name, constructor atom, and
/// the payload wire shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionArm {
    pub outcome: String,
    pub ctor: AtomRef,
    pub payload: WireDesc,
}

/// A registered type shape. `tag`/`ctor` atoms are pre-snaked at lowering
/// time; the engine never derives them from JSON names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeShape {
    Record {
        name: String,
        tag: AtomRef,
        fields: Vec<FieldShape>,
    },
    Enum {
        name: String,
        /// `(constructor atom, JSON variant name)` in declaration order.
        variants: Vec<(AtomRef, String)>,
    },
    Union {
        name: String,
        arms: Vec<UnionArm>,
    },
}

impl TypeShape {
    /// The registry name (the `WireDesc::Ref` target).
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Record { name, .. } | Self::Enum { name, .. } | Self::Union { name, .. } => name,
        }
    }
}

/// A beamr `Literal` shape (the literal pool holds only these).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirLiteral {
    Integer(i64),
    /// S3: the source lexeme is retained so byte-stable `LitT` floats can pin
    /// the parse against the reference's parse of the same lexeme.
    Float {
        lexeme: String,
    },
    Atom(AtomRef),
    Binary(Vec<u8>),
    Tuple(Vec<MirLiteral>),
    Nil,
    List(Vec<MirLiteral>),
}
