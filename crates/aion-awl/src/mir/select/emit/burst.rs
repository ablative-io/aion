//! Explicit-label value bursts for comparisons and boolean operators. Every
//! test failure targets a real local label; label zero is never used as an
//! accidental exception/trap path.

use beamr::loader::decode::{ComparisonOp, Instruction, Operand};

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
