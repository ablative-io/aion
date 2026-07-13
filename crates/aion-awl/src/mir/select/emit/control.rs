//! S17 tail emission. Conditional and enum control constructs terminate their
//! blocks; each selected arm recursively emits its own terminal tail.

use beamr::loader::decode::{ComparisonOp, Instruction, Operand};

use super::{BranchBlock, Emit, SelectError, TailKind, TestKind};

impl Emit<'_, '_> {
    fn branch_block(&mut self, block: &BranchBlock) -> Result<(), SelectError> {
        for step in &block.steps {
            self.step(step)?;
        }
        self.tail(&block.tail)
    }

    fn test_branches(
        &mut self,
        test: &TestKind,
        true_label: u32,
        false_label: u32,
    ) -> Result<(), SelectError> {
        match test {
            TestKind::IsTrue(src) => {
                let left = self.operand(src)?;
                let true_atom = self.builder.atom("true");
                self.push(Instruction::Comparison {
                    op: ComparisonOp::EqExact,
                    fail: Operand::Label(false_label),
                    left,
                    right: Operand::Atom(Some(true_atom)),
                });
            }
            TestKind::Cmp { op, lhs, rhs } => {
                self.comparison(*op, lhs, rhs, false_label)?;
            }
            TestKind::IsTagged { value, tag, arity } => {
                let value = self.operand(value)?;
                self.push(Instruction::IsTaggedTuple {
                    fail: Operand::Label(false_label),
                    value,
                    arity: Operand::Unsigned(u64::from(*arity)),
                    tag: Operand::Atom(Some(*tag)),
                });
            }
            TestKind::Not(inner) => {
                return self.test_branches(inner, false_label, true_label);
            }
        }
        self.push(Instruction::Jump {
            target: Operand::Label(true_label),
        });
        Ok(())
    }

    pub(super) fn tail(&mut self, tail: &TailKind) -> Result<(), SelectError> {
        match tail {
            TailKind::Return(src) => {
                self.reload(src, 0)?;
                if self.framed {
                    let exit = self.lexit();
                    self.push(Instruction::Jump {
                        target: Operand::Label(exit),
                    });
                } else {
                    self.push(Instruction::Return);
                }
            }
            TailKind::TailImport {
                import,
                arity,
                args,
            } => {
                self.marshal(args)?;
                if self.framed {
                    self.push(Instruction::CallExtLast {
                        arity: Operand::Unsigned(u64::from(*arity)),
                        import: Operand::Unsigned(*import as u64),
                        deallocate: Operand::Unsigned(u64::from(self.frame_size)),
                    });
                } else {
                    self.push(Instruction::CallExtOnly {
                        arity: Operand::Unsigned(u64::from(*arity)),
                        import: Operand::Unsigned(*import as u64),
                    });
                }
            }
            TailKind::TailLocal { label, arity, args } => {
                self.marshal(args)?;
                if self.framed {
                    self.push(Instruction::CallLast {
                        arity: Operand::Unsigned(u64::from(*arity)),
                        label: Operand::Label(*label),
                        deallocate: Operand::Unsigned(u64::from(self.frame_size)),
                    });
                } else {
                    self.push(Instruction::CallOnly {
                        arity: Operand::Unsigned(u64::from(*arity)),
                        label: Operand::Label(*label),
                    });
                }
            }
            TailKind::If {
                test,
                then_block,
                else_block,
            } => {
                let then_label = self.builder.fresh_label();
                let else_label = self.builder.fresh_label();
                self.test_branches(test, then_label, else_label)?;
                self.push(Instruction::Label { label: then_label });
                self.branch_block(then_block)?;
                self.push(Instruction::Label { label: else_label });
                self.branch_block(else_block)?;
            }
            TailKind::SelectEnum { subject, arms } => {
                let fail = self.builder.fresh_label();
                let labels: Vec<u32> = arms.iter().map(|_| self.builder.fresh_label()).collect();
                let mut choices = Vec::with_capacity(arms.len() * 2);
                for ((tag, _), label) in arms.iter().zip(&labels) {
                    choices.push(Operand::Atom(Some(*tag)));
                    choices.push(Operand::Label(*label));
                }
                let value = self.operand(subject)?;
                self.push(Instruction::SelectVal {
                    value: value.clone(),
                    fail: Operand::Label(fail),
                    list: Operand::List(choices),
                });
                for ((_, block), label) in arms.iter().zip(labels) {
                    self.push(Instruction::Label { label });
                    self.branch_block(block)?;
                }
                self.push(Instruction::Label { label: fail });
                self.push(Instruction::CaseEnd { value });
            }
        }
        Ok(())
    }
}
