//! The BC-3 selection IR: a resolved, register-free function body that both
//! flow functions and expanded template shells (AWL-BC-IR.md §11.4 "one
//! selector") lower into, and that the single burst emitter (`emit.rs`)
//! consumes. Atoms/literals/imports/lambdas are already interned to pool
//! indices here, so emission is a pure register-allocation + instruction walk.

use beamr::atom::Atom;

use crate::mir::Var;

/// A resolved value source (no registers — the emitter assigns X/Y).
#[derive(Debug, Clone)]
pub(super) enum Src {
    Var(Var),
    /// Index into the module literal pool.
    Lit(usize),
    Int(i64),
    Atom(Atom),
    /// The `nil`/`[]` atom (`Operand::Atom(None)`).
    Nil,
}

/// How a `JsonObj` pair value is encoded to a `json.Json` value.
#[derive(Debug, Clone, Copy)]
pub(super) enum Via {
    /// An SDK leaf `to_json` import (`aion@awl@codec:<leaf>_to_json/1`).
    Import(usize),
    /// A module-local `to_json` function, called at its body label.
    Local(u32),
}

/// One `json.object` pair: a name binary (literal-pool index) and its encoded
/// value.
#[derive(Debug, Clone)]
pub(super) struct JsonPair {
    pub(super) name_lit: usize,
    pub(super) value: Src,
    pub(super) via: Via,
}

/// One resolved statement burst.
#[derive(Debug, Clone)]
pub(super) enum Step {
    /// `get_tuple_element base, index -> dst` (index is the BEAM element index,
    /// tag at 0 — the MIR already stores element indices, not 0-based ordinals).
    FieldGet { dst: Var, base: Var, index: u16 },
    /// `put_tuple2 dst, [tag, args...]`; zero args ⇒ bare tag atom `move`.
    Record { dst: Var, tag: Atom, args: Vec<Src> },
    /// `put_list` chain from nil.
    ListNew { dst: Var, items: Vec<Src> },
    /// `call_ext import` (a used `RuntimeFn`).
    CallImport {
        dst: Option<Var>,
        import: usize,
        arity: u8,
        args: Vec<Src>,
    },
    /// `call label` (a module-local function's body label).
    CallLocal {
        dst: Option<Var>,
        label: u32,
        arity: u8,
        args: Vec<Src>,
    },
    /// `make_fun2 lambda` + `FunT` entry (captures marshaled to `x0..free-1`).
    MakeClosure {
        dst: Var,
        lambda: usize,
        captures: Vec<Src>,
    },
    /// Flattened `result.try` (§2.2): `is_tagged_tuple` on the result, extract
    /// the ok payload, fail branch to the shared exit.
    TryBind {
        dst: Var,
        result: Var,
        ok_atom: Atom,
    },
    /// `json.object([...])` assembled from encoded pairs. `object_import` is the
    /// import-pool index of `gleam@json:object/1`.
    JsonObj {
        dst: Var,
        pairs: Vec<JsonPair>,
        object_import: usize,
    },
}

/// The block terminator.
#[derive(Debug, Clone)]
pub(super) enum TailKind {
    Return(Src),
    /// `call_ext_last import` (shells) / `call_ext_only` (frameless).
    TailImport {
        import: usize,
        arity: u8,
        args: Vec<Src>,
    },
    /// `call_last label` (regions) / `call_only` (frameless).
    TailLocal {
        label: u32,
        arity: u8,
        args: Vec<Src>,
    },
}

/// A resolved function body ready for register allocation + emission.
#[derive(Debug, Clone)]
pub(super) struct Body {
    /// The physical parameters, in `x0..x(p-1)` order.
    pub(super) params: Vec<Var>,
    pub(super) steps: Vec<Step>,
    pub(super) tail: TailKind,
    /// The function's own name atom, arity, and two labels — for the header.
    pub(super) name: Atom,
    pub(super) module: Atom,
    pub(super) arity: u8,
    pub(super) entry_label: u32,
    pub(super) code_label: u32,
}
