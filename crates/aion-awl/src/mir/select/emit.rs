//! The single burst emitter (AWL-BC-IR.md §11.2–§11.4): a resolved [`Body`] →
//! a validate-clean `Instruction` stream.
//!
//! Register model (the R5 fallback realized conservatively, recorded in §11.6):
//! every function with parameters or defined vars is **framed** — each var homed
//! in its own Y slot, so Y is touched ONLY by `move` (reload before a use, store
//! right after a def) and no value ever lives in X across a call. Frames open
//! with `Allocate F` and close at a single shared `Lexit: Deallocate F; Return`
//! (R7); tail calls deallocate via their own operand. Functions with neither a
//! parameter nor a var are frameless (a tail over immediates only). Every `Live`
//! operand is the exact X high-water so GC never clears a needed register.

use std::collections::HashMap;

use beamr::loader::decode::{Instruction, Operand};

use crate::mir::Var;

use super::builder::Builder;
use super::error::SelectError;
use super::ir::{Body, JsonPair, Src, Step, TailKind, Via};

/// Emits one function body into a flat instruction stream.
pub(super) fn emit_body(
    builder: &mut Builder<'_>,
    body: &Body,
) -> Result<Vec<Instruction>, SelectError> {
    let plan = FramePlan::new(body)?;
    let mut emit = Emit {
        builder,
        code: Vec::new(),
        homes: plan.homes,
        acc_slot: plan.acc_slot,
        frame_size: plan.frame_size,
        framed: plan.frame_size > 0,
        param_count: u32::try_from(body.params.len()).unwrap_or(u32::MAX),
        lexit: None,
    };
    emit.header(body);
    emit.prologue();
    for step in &body.steps {
        emit.step(step)?;
    }
    emit.tail(&body.tail)?;
    emit.exit_block();
    Ok(emit.code)
}

/// The Y-slot assignment for a body: params first, then defined vars in step
/// order, then one `JsonObj` accumulator temp (§11.2) when any object has ≥2
/// pairs.
struct FramePlan {
    homes: HashMap<Var, u32>,
    frame_size: u32,
    acc_slot: Option<u32>,
}

impl FramePlan {
    fn new(body: &Body) -> Result<Self, SelectError> {
        let mut homes = HashMap::new();
        let mut slot = 0_u32;
        let assign = |var: Var, homes: &mut HashMap<Var, u32>, slot: &mut u32| {
            homes.entry(var).or_insert_with(|| {
                let here = *slot;
                *slot += 1;
                here
            });
        };
        for param in &body.params {
            assign(*param, &mut homes, &mut slot);
        }
        let mut needs_acc = false;
        for step in &body.steps {
            if let Some(dst) = step.defined() {
                assign(dst, &mut homes, &mut slot);
            }
            if let Step::JsonObj { pairs, .. } = step {
                needs_acc = needs_acc || pairs.len() >= 2;
            }
        }
        let acc_slot = needs_acc.then(|| {
            let here = slot;
            slot += 1;
            here
        });
        if slot >= 256 {
            return Err(SelectError::OutOfRange {
                what: format!("frame size {slot} exceeds the Y-register cap"),
            });
        }
        Ok(Self {
            homes,
            frame_size: slot,
            acc_slot,
        })
    }
}

impl Step {
    fn defined(&self) -> Option<Var> {
        match self {
            Self::FieldGet { dst, .. }
            | Self::Record { dst, .. }
            | Self::ListNew { dst, .. }
            | Self::MakeClosure { dst, .. }
            | Self::TryBind { dst, .. }
            | Self::JsonObj { dst, .. } => Some(*dst),
            Self::CallImport { dst, .. } | Self::CallLocal { dst, .. } => *dst,
        }
    }
}

struct Emit<'b, 'm> {
    builder: &'b mut Builder<'m>,
    code: Vec<Instruction>,
    homes: HashMap<Var, u32>,
    acc_slot: Option<u32>,
    frame_size: u32,
    framed: bool,
    param_count: u32,
    lexit: Option<u32>,
}

impl Emit<'_, '_> {
    fn push(&mut self, instruction: Instruction) {
        self.code.push(instruction);
    }

    fn header(&mut self, body: &Body) {
        self.push(Instruction::Label {
            label: body.entry_label,
        });
        self.push(Instruction::FuncInfo {
            module: Operand::Atom(Some(body.module)),
            function: Operand::Atom(Some(body.name)),
            arity: Operand::Unsigned(u64::from(body.arity)),
        });
        self.push(Instruction::Label {
            label: body.code_label,
        });
    }

    fn prologue(&mut self) {
        if !self.framed {
            return;
        }
        self.push(Instruction::Allocate {
            stack_need: Operand::Unsigned(u64::from(self.frame_size)),
            live: Operand::Unsigned(u64::from(self.param_count)),
        });
        for index in 0..self.param_count {
            self.push(Instruction::Move {
                source: Operand::X(index),
                destination: Operand::Y(index),
            });
        }
    }

    fn home(&self, var: Var) -> Result<u32, SelectError> {
        self.homes
            .get(&var)
            .copied()
            .ok_or_else(|| SelectError::invariant(format!("var {} has no Y home", var.0)))
    }

    /// The operand a source occupies when reloaded (immediates are inline; a var
    /// is its Y home, requiring a `move` to X before use).
    fn immediate(src: &Src) -> Option<Operand> {
        match src {
            Src::Var(_) => None,
            Src::Lit(index) => Some(Operand::Literal(*index)),
            Src::Int(value) => Some(Operand::Integer(*value)),
            Src::Atom(atom) => Some(Operand::Atom(Some(*atom))),
            Src::Nil => Some(Operand::Atom(None)),
        }
    }

    /// Reload a source into X register `x` (var → `move y->x`; immediate →
    /// `move imm->x`).
    fn reload(&mut self, src: &Src, x: u32) -> Result<(), SelectError> {
        let source = match src {
            Src::Var(var) => Operand::Y(self.home(*var)?),
            Src::Lit(index) => Operand::Literal(*index),
            Src::Int(value) => Operand::Integer(*value),
            Src::Atom(atom) => Operand::Atom(Some(*atom)),
            Src::Nil => Operand::Atom(None),
        };
        self.push(Instruction::Move {
            source,
            destination: Operand::X(x),
        });
        Ok(())
    }

    /// Marshal call/closure args into `x0..x(k-1)`.
    fn marshal(&mut self, args: &[Src]) -> Result<u8, SelectError> {
        let arity = u8::try_from(args.len()).map_err(|_| SelectError::OutOfRange {
            what: format!("call arity {} exceeds 255", args.len()),
        })?;
        for (index, arg) in args.iter().enumerate() {
            self.reload(arg, u32::try_from(index).unwrap_or(u32::MAX))?;
        }
        Ok(arity)
    }

    /// Store the X0 result of a burst into a var's Y home.
    fn store(&mut self, dst: Var) -> Result<(), SelectError> {
        let home = self.home(dst)?;
        self.push(Instruction::Move {
            source: Operand::X(0),
            destination: Operand::Y(home),
        });
        Ok(())
    }

    fn lexit(&mut self) -> u32 {
        *self.lexit.get_or_insert_with(|| self.builder.fresh_label())
    }

    fn step(&mut self, step: &Step) -> Result<(), SelectError> {
        match step {
            Step::FieldGet { dst, base, index } => {
                let home = self.home(*base)?;
                self.push(Instruction::Move {
                    source: Operand::Y(home),
                    destination: Operand::X(0),
                });
                self.push(Instruction::GetTupleElement {
                    source: Operand::X(0),
                    index: Operand::Unsigned(u64::from(*index)),
                    destination: Operand::X(1),
                });
                self.push(Instruction::Move {
                    source: Operand::X(1),
                    destination: Operand::Y(self.home(*dst)?),
                });
            }
            Step::Record { dst, tag, args } => self.record(*dst, *tag, args)?,
            Step::ListNew { dst, items } => self.list_new(*dst, items)?,
            Step::CallImport {
                dst,
                import,
                arity,
                args,
            } => {
                let marshaled = self.marshal(args)?;
                debug_assert_eq!(marshaled, *arity);
                self.push(Instruction::CallExt {
                    arity: Operand::Unsigned(u64::from(*arity)),
                    import: Operand::Unsigned(*import as u64),
                });
                if let Some(dst) = dst {
                    self.store(*dst)?;
                }
            }
            Step::CallLocal {
                dst,
                label,
                arity,
                args,
            } => {
                self.marshal(args)?;
                self.push(Instruction::Call {
                    arity: Operand::Unsigned(u64::from(*arity)),
                    label: Operand::Label(*label),
                });
                if let Some(dst) = dst {
                    self.store(*dst)?;
                }
            }
            Step::MakeClosure {
                dst,
                lambda,
                captures,
            } => {
                self.marshal(captures)?;
                self.push(Instruction::MakeFun {
                    operands: vec![Operand::Unsigned(*lambda as u64)],
                });
                self.store(*dst)?;
            }
            Step::TryBind {
                dst,
                result,
                ok_atom,
            } => {
                let fail = self.lexit();
                let home = self.home(*result)?;
                self.push(Instruction::Move {
                    source: Operand::Y(home),
                    destination: Operand::X(0),
                });
                self.push(Instruction::IsTaggedTuple {
                    fail: Operand::Label(fail),
                    value: Operand::X(0),
                    arity: Operand::Unsigned(2),
                    tag: Operand::Atom(Some(*ok_atom)),
                });
                self.push(Instruction::GetTupleElement {
                    source: Operand::X(0),
                    index: Operand::Unsigned(1),
                    destination: Operand::X(1),
                });
                self.push(Instruction::Move {
                    source: Operand::X(1),
                    destination: Operand::Y(self.home(*dst)?),
                });
            }
            Step::JsonObj {
                dst,
                pairs,
                object_import,
            } => self.json_obj(*dst, pairs, *object_import)?,
        }
        Ok(())
    }

    fn record(
        &mut self,
        dst: Var,
        tag: beamr::atom::Atom,
        args: &[Src],
    ) -> Result<(), SelectError> {
        if args.is_empty() {
            self.push(Instruction::Move {
                source: Operand::Atom(Some(tag)),
                destination: Operand::X(0),
            });
            return self.store(dst);
        }
        let arity = args.len() + 1;
        self.push(Instruction::TestHeap {
            heap_need: Operand::Unsigned((arity + 1) as u64),
            live: Operand::Unsigned(0),
        });
        let mut elements = vec![Operand::Atom(Some(tag))];
        let mut next_x = 1_u32;
        for arg in args {
            if let Some(operand) = Self::immediate(arg) {
                elements.push(operand);
            } else {
                self.reload(arg, next_x)?;
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

    fn list_new(&mut self, dst: Var, items: &[Src]) -> Result<(), SelectError> {
        self.push(Instruction::TestHeap {
            heap_need: Operand::Unsigned((items.len() * 2) as u64),
            live: Operand::Unsigned(0),
        });
        let mut tail = Operand::Atom(None);
        for item in items.iter().rev() {
            let head = if let Some(operand) = Self::immediate(item) {
                operand
            } else {
                self.reload(item, 1)?;
                Operand::X(1)
            };
            self.push(Instruction::PutList {
                head,
                tail,
                destination: Operand::X(0),
            });
            tail = Operand::X(0);
        }
        self.store(dst)
    }

    /// `json.object([...])` (§11.4): each pair's value is encoded, wrapped in a
    /// `{name, encoded}` tuple, and consed onto the accumulator (Y-homed while a
    /// later pair's encode call could clobber X). Pairs are processed in reverse
    /// so the finished list is in declaration order.
    fn json_obj(
        &mut self,
        dst: Var,
        pairs: &[JsonPair],
        object_import: usize,
    ) -> Result<(), SelectError> {
        if pairs.is_empty() {
            self.push(Instruction::Move {
                source: Operand::Atom(None),
                destination: Operand::X(0),
            });
        } else {
            let acc = self.acc_slot;
            let last_index = pairs.len() - 1;
            for (position, pair) in pairs.iter().enumerate().rev() {
                self.reload(&pair.value, 0)?;
                match pair.via {
                    Via::Import(import) => self.push(Instruction::CallExt {
                        arity: Operand::Unsigned(1),
                        import: Operand::Unsigned(import as u64),
                    }),
                    Via::Local(label) => self.push(Instruction::Call {
                        arity: Operand::Unsigned(1),
                        label: Operand::Label(label),
                    }),
                }
                // tuple {name, encoded} (3 words) + cons (2 words).
                self.push(Instruction::TestHeap {
                    heap_need: Operand::Unsigned(5),
                    live: Operand::Unsigned(1),
                });
                self.push(Instruction::PutTuple2 {
                    destination: Operand::X(1),
                    elements: Operand::List(vec![Operand::Literal(pair.name_lit), Operand::X(0)]),
                });
                let tail = if position == last_index {
                    Operand::Atom(None)
                } else {
                    let slot = acc.ok_or_else(|| {
                        SelectError::invariant("json_obj accumulator slot missing")
                    })?;
                    self.push(Instruction::Move {
                        source: Operand::Y(slot),
                        destination: Operand::X(2),
                    });
                    Operand::X(2)
                };
                self.push(Instruction::PutList {
                    head: Operand::X(1),
                    tail,
                    destination: Operand::X(0),
                });
                if position != 0 {
                    let slot = acc.ok_or_else(|| {
                        SelectError::invariant("json_obj accumulator slot missing")
                    })?;
                    self.push(Instruction::Move {
                        source: Operand::X(0),
                        destination: Operand::Y(slot),
                    });
                }
            }
        }
        self.push(Instruction::CallExt {
            arity: Operand::Unsigned(1),
            import: Operand::Unsigned(object_import as u64),
        });
        self.store(dst)
    }

    fn tail(&mut self, tail: &TailKind) -> Result<(), SelectError> {
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
        }
        Ok(())
    }

    fn exit_block(&mut self) {
        if let Some(exit) = self.lexit {
            self.push(Instruction::Label { label: exit });
            self.push(Instruction::Deallocate {
                words: Operand::Unsigned(u64::from(self.frame_size)),
            });
            self.push(Instruction::Return);
        }
    }
}
