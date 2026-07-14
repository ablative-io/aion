//! Explicit-label value bursts for comparisons, boolean operators, and the
//! loop-counter arithmetic/tuple ops. Every TEST failure targets a real local
//! label; label zero is never an accidental trap path — the one deliberate
//! fail-0 is `Increment`'s `gc_bif2`, where a non-integer must raise
//! `badarith` exactly as Gleam's `+` would.

use beamr::loader::decode::{BifOp, ComparisonOp, Instruction, Operand, TypeTestOp};

use crate::mir::RuntimeFn;
use crate::mir::{BoolBin, CmpOp, Var};

use super::{Emit, SelectError, Src};

impl Emit<'_, '_> {
    pub(super) fn operand(&self, src: &Src) -> Result<Operand, SelectError> {
        Ok(match src {
            Src::Var(var) => Operand::Y(self.home(*var)?),
            Src::Lit(index) => Operand::Literal(*index),
            Src::Int(value) => Operand::Integer(*value),
            Src::Atom(atom) => Operand::Atom(Some(*atom)),
            Src::Nil => Operand::Atom(None),
        })
    }

    pub(super) fn comparison(
        &mut self,
        op: CmpOp,
        lhs: &Src,
        rhs: &Src,
        fail: u32,
    ) -> Result<(), SelectError> {
        let (op, swap) = comparison_op(op);
        let (left, right) = if swap { (rhs, lhs) } else { (lhs, rhs) };
        let left = self.operand(left)?;
        let right = self.operand(right)?;
        self.push(Instruction::Comparison {
            op,
            fail: Operand::Label(fail),
            left,
            right,
        });
        Ok(())
    }

    fn truth_test(&mut self, src: &Src, fail: u32) -> Result<(), SelectError> {
        let left = self.operand(src)?;
        let true_atom = self.builder.atom("true");
        self.push(Instruction::Comparison {
            op: ComparisonOp::EqExact,
            fail: Operand::Label(fail),
            left,
            right: Operand::Atom(Some(true_atom)),
        });
        Ok(())
    }

    fn finish_bool(&mut self, dst: Var, false_label: u32, done: u32) -> Result<(), SelectError> {
        let true_atom = self.builder.atom("true");
        self.push(Instruction::Move {
            source: Operand::Atom(Some(true_atom)),
            destination: Operand::X(0),
        });
        self.push(Instruction::Jump {
            target: Operand::Label(done),
        });
        self.push(Instruction::Label { label: false_label });
        let false_atom = self.builder.atom("false");
        self.push(Instruction::Move {
            source: Operand::Atom(Some(false_atom)),
            destination: Operand::X(0),
        });
        self.push(Instruction::Label { label: done });
        self.store(dst)
    }

    pub(super) fn cmp_value(
        &mut self,
        dst: Var,
        op: CmpOp,
        lhs: &Src,
        rhs: &Src,
    ) -> Result<(), SelectError> {
        let false_label = self.builder.fresh_label();
        let done = self.builder.fresh_label();
        self.comparison(op, lhs, rhs, false_label)?;
        self.finish_bool(dst, false_label, done)
    }

    pub(super) fn bool_value(
        &mut self,
        dst: Var,
        op: BoolBin,
        lhs: &Src,
        rhs: &Src,
    ) -> Result<(), SelectError> {
        let false_label = self.builder.fresh_label();
        let done = self.builder.fresh_label();
        match op {
            BoolBin::And => {
                self.truth_test(lhs, false_label)?;
                self.truth_test(rhs, false_label)?;
            }
            BoolBin::Or => {
                let rhs_label = self.builder.fresh_label();
                let true_label = self.builder.fresh_label();
                self.truth_test(lhs, rhs_label)?;
                self.push(Instruction::Jump {
                    target: Operand::Label(true_label),
                });
                self.push(Instruction::Label { label: rhs_label });
                self.truth_test(rhs, false_label)?;
                self.push(Instruction::Label { label: true_label });
            }
        }
        self.finish_bool(dst, false_label, done)
    }

    pub(super) fn not_value(&mut self, dst: Var, src: &Src) -> Result<(), SelectError> {
        let true_label = self.builder.fresh_label();
        let done = self.builder.fresh_label();
        self.truth_test(src, true_label)?;
        let false_atom = self.builder.atom("false");
        self.push(Instruction::Move {
            source: Operand::Atom(Some(false_atom)),
            destination: Operand::X(0),
        });
        self.push(Instruction::Jump {
            target: Operand::Label(done),
        });
        self.push(Instruction::Label { label: true_label });
        let true_atom = self.builder.atom("true");
        self.push(Instruction::Move {
            source: Operand::Atom(Some(true_atom)),
            destination: Operand::X(0),
        });
        self.push(Instruction::Label { label: done });
        self.store(dst)
    }
}

impl Emit<'_, '_> {
    /// Untagged tuple construction (`#(value, count)`): `record` minus the
    /// tag element.
    pub(super) fn tuple_new(&mut self, dst: Var, items: &[Src]) -> Result<(), SelectError> {
        self.push(Instruction::TestHeap {
            heap_need: Operand::Unsigned((items.len() + 1) as u64),
            live: Operand::Unsigned(0),
        });
        let mut elements = Vec::with_capacity(items.len());
        let mut next_x = 1_u32;
        for item in items {
            if let Some(operand) = Self::immediate(item) {
                elements.push(operand);
            } else {
                self.reload(item, next_x)?;
                elements.push(Operand::X(next_x));
                next_x += 1;
            }
        }
        self.push(Instruction::PutTuple2 {
            destination: Operand::X(0),
            elements: Operand::List(elements),
        });
        self.store(dst)
    }

    /// The loop-counter increment: `gc_bif2 erlang:'+'(count, 1)`. The BIF
    /// target needs a real `ImpT` row (beamr resolves `Bif` through the import
    /// table, like OTP `.beam` files); fail label 0 raises `badarith`.
    pub(super) fn increment(&mut self, dst: Var, src: Var) -> Result<(), SelectError> {
        let import = self.builder.import(RuntimeFn::IntAdd)?;
        let home = self.home(src)?;
        self.push(Instruction::Move {
            source: Operand::Y(home),
            destination: Operand::X(0),
        });
        self.push(Instruction::Bif {
            op: BifOp::GcBif2,
            operands: vec![
                Operand::Label(0),
                Operand::Unsigned(1),
                Operand::Unsigned(import as u64),
                Operand::X(0),
                Operand::Integer(1),
                Operand::X(0),
            ],
        });
        self.store(dst)
    }
}

fn comparison_op(op: CmpOp) -> (ComparisonOp, bool) {
    match op {
        CmpOp::Eq => (ComparisonOp::EqExact, false),
        CmpOp::Ne => (ComparisonOp::NeExact, false),
        CmpOp::Lt | CmpOp::FLt => (ComparisonOp::Lt, false),
        CmpOp::Ge | CmpOp::FGe => (ComparisonOp::Ge, false),
        CmpOp::Le | CmpOp::FLe => (ComparisonOp::Ge, true),
        CmpOp::Gt | CmpOp::FGt => (ComparisonOp::Lt, true),
    }
}

impl Emit<'_, '_> {
    /// One cons cell onto an existing tail (`[head, ..tail]` — the sequential
    /// fork fold's accumulator prepend).
    pub(super) fn cons(&mut self, dst: Var, head: &Src, tail: &Src) -> Result<(), SelectError> {
        self.push(Instruction::TestHeap {
            heap_need: Operand::Unsigned(2),
            live: Operand::Unsigned(0),
        });
        let head_operand = if let Some(operand) = Self::immediate(head) {
            operand
        } else {
            self.reload(head, 1)?;
            Operand::X(1)
        };
        let tail_operand = if let Some(operand) = Self::immediate(tail) {
            operand
        } else {
            self.reload(tail, 2)?;
            Operand::X(2)
        };
        self.push(Instruction::PutList {
            head: head_operand,
            tail: tail_operand,
            destination: Operand::X(0),
        });
        self.store(dst)
    }

    /// `let assert [a, b, …] = list` — unrolled head/tail extraction with an
    /// exact-length `is_nil` check; any mismatch is an explicit `badmatch`
    /// trap (the Gleam `let assert` shape, mirroring `assert_some`).
    pub(super) fn assert_list(
        &mut self,
        binds: &[Option<Var>],
        list: Var,
    ) -> Result<(), SelectError> {
        let fail = self.builder.fresh_label();
        let done = self.builder.fresh_label();
        self.push(Instruction::Move {
            source: Operand::Y(self.home(list)?),
            destination: Operand::X(0),
        });
        for bind in binds {
            self.push(Instruction::TypeTest {
                op: TypeTestOp::IsNonemptyList,
                fail: Operand::Label(fail),
                value: Operand::X(0),
            });
            self.push(Instruction::GetList {
                source: Operand::X(0),
                head: Operand::X(1),
                tail: Operand::X(2),
            });
            if let Some(var) = bind {
                self.push(Instruction::Move {
                    source: Operand::X(1),
                    destination: Operand::Y(self.home(*var)?),
                });
            }
            self.push(Instruction::Move {
                source: Operand::X(2),
                destination: Operand::X(0),
            });
        }
        self.push(Instruction::TypeTest {
            op: TypeTestOp::IsNil,
            fail: Operand::Label(fail),
            value: Operand::X(0),
        });
        self.push(Instruction::Jump {
            target: Operand::Label(done),
        });
        self.push(Instruction::Label { label: fail });
        // Trap on the SUBJECT list, not the walked tail X0 happens to hold —
        // `let assert [...] = subject` reports the whole subject (BC-2b-5
        // carried fix; the walked-tail operand misattributed the mismatch).
        self.push(Instruction::Move {
            source: Operand::Y(self.home(list)?),
            destination: Operand::X(0),
        });
        self.push(Instruction::Badmatch {
            value: Operand::X(0),
        });
        self.push(Instruction::Label { label: done });
        Ok(())
    }
}

impl Emit<'_, '_> {
    /// `call_fun` of a closure value: args in `x0..k-1`, the fun in `x(k)`
    /// (the T-ACTRAW input-codec `encode` invocation).
    pub(super) fn call_fun(
        &mut self,
        dst: Option<Var>,
        fun: &Src,
        args: &[Src],
    ) -> Result<(), SelectError> {
        let arity = self.marshal(args)?;
        self.reload(fun, u32::from(arity))?;
        self.push(Instruction::CallFun {
            arity: Operand::Unsigned(u64::from(arity)),
        });
        if let Some(dst) = dst {
            self.store(dst)?;
        }
        Ok(())
    }
}
