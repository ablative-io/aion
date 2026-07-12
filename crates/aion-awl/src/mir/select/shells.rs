//! Template-shell expansion (AWL-BC-IR.md §2.4 / §11.4 "Shell expansion — one
//! selector"): each `TemplateFn` expands into a resolved [`Body`] in the same
//! op vocabulary flow functions use, so a single emitter owns every function.
//! Recipes are name-substitution-only and mint at most one closure (S8); T-ACT
//! additionally references the shared dead-body lambda (T-DEAD), minted here.

use crate::mir::runtime::RuntimeFn;
use crate::mir::{CodecRef, FnOrigin, FnRef, MirModule, Span, TemplateFn, TypeShape};

use super::builder::Builder;
use super::error::SelectError;
use super::ir::{Body, Src, Step, TailKind};

/// The `FnRef` of the module's `execute/1` (the value passed to `run/4` and
/// `define/5`).
pub(super) fn find_execute(module: &MirModule) -> Result<FnRef, SelectError> {
    module
        .functions
        .iter()
        .position(|function| matches!(function.origin(), FnOrigin::Execute))
        .map(|index| FnRef(u32::try_from(index).unwrap_or(u32::MAX)))
        .ok_or_else(|| SelectError::invariant("module has no execute/1"))
}

struct Shell<'b, 'm> {
    builder: &'b mut Builder<'m>,
    next_var: u32,
    steps: Vec<Step>,
}

impl<'b, 'm> Shell<'b, 'm> {
    fn new(builder: &'b mut Builder<'m>) -> Self {
        Self {
            builder,
            next_var: 0,
            steps: Vec::new(),
        }
    }

    fn fresh(&mut self) -> crate::mir::Var {
        let var = crate::mir::Var(self.next_var);
        self.next_var += 1;
        var
    }

    /// A 0-arity codec call (`make_closure`-free), storing into a fresh var.
    fn codec(&mut self, codec: &CodecRef) -> Result<crate::mir::Var, SelectError> {
        let dst = self.fresh();
        let step = match codec {
            CodecRef::Local(reference) => Step::CallLocal {
                dst: Some(dst),
                label: Builder::fn_labels(*reference).body,
                arity: 0,
                args: Vec::new(),
            },
            CodecRef::SdkNil => self.import_call(dst, RuntimeFn::NilCodec)?,
            CodecRef::SdkLeaf(leaf) => self.import_call(dst, RuntimeFn::LeafCodec(*leaf))?,
        };
        self.steps.push(step);
        Ok(dst)
    }

    /// A 0-arity codec call to a module-local composer given by `FnRef`.
    fn local_codec(&mut self, reference: FnRef) -> crate::mir::Var {
        let dst = self.fresh();
        self.steps.push(Step::CallLocal {
            dst: Some(dst),
            label: Builder::fn_labels(reference).body,
            arity: 0,
            args: Vec::new(),
        });
        dst
    }

    fn import_call(
        &mut self,
        dst: crate::mir::Var,
        callee: RuntimeFn,
    ) -> Result<Step, SelectError> {
        Ok(Step::CallImport {
            dst: Some(dst),
            import: self.builder.import(callee)?,
            arity: 0,
            args: Vec::new(),
        })
    }

    /// `make_fun2(execute)` — a 0-free closure over the exported `execute/1`.
    fn make_execute(&mut self, execute: FnRef) -> Result<crate::mir::Var, SelectError> {
        let name = self.builder.atom("execute");
        let arity = execute_arity(self.builder.module, execute)?;
        let label = Builder::fn_labels(execute).body;
        let lambda = self.builder.lambda(name, arity, label, 0);
        let dst = self.fresh();
        self.steps.push(Step::MakeClosure {
            dst,
            lambda,
            captures: Vec::new(),
        });
        Ok(dst)
    }

    fn finish(self, tail: TailKind, header: &Header) -> Body {
        Body {
            params: (0..header.param_count).map(crate::mir::Var).collect(),
            steps: self.steps,
            tail,
            name: header.name,
            module: header.module,
            arity: header.arity,
            entry_label: header.entry_label,
            code_label: header.code_label,
        }
    }
}

struct Header {
    name: beamr::atom::Atom,
    module: beamr::atom::Atom,
    arity: u8,
    param_count: u32,
    entry_label: u32,
    code_label: u32,
}

fn execute_arity(module: &MirModule, execute: FnRef) -> Result<u8, SelectError> {
    let function = module
        .function(execute)
        .ok_or_else(|| SelectError::invariant("execute ref out of range"))?;
    u8::try_from(MirModule::arity(function)).map_err(|_| SelectError::OutOfRange {
        what: "execute arity".to_owned(),
    })
}

/// Expand one template shell into a resolved body (dispatch; each recipe is a
/// helper below).
pub(super) fn lower_shell(
    builder: &mut Builder<'_>,
    name: &str,
    arity: u8,
    template: &TemplateFn,
    reference: FnRef,
    execute: FnRef,
) -> Result<Body, SelectError> {
    let module_atom = builder.atom(&builder.module.name.clone());
    let name_atom = builder.atom(name);
    let labels = Builder::fn_labels(reference);
    let header = Header {
        name: name_atom,
        module: module_atom,
        arity,
        param_count: u32::from(arity),
        entry_label: labels.entry,
        code_label: labels.body,
    };

    match template {
        TemplateFn::Run {
            input_codec,
            output_codec,
        } => shell_run(builder, *input_codec, output_codec, execute, &header),
        TemplateFn::Definition {
            workflow_name,
            input_codec,
            output_codec,
        } => shell_definition(
            builder,
            workflow_name,
            *input_codec,
            output_codec,
            execute,
            &header,
        ),
        TemplateFn::Execute {
            input_fields,
            entry,
            entry_args,
        } => shell_execute(builder, input_fields.len(), *entry, entry_args, &header),
        TemplateFn::ActivityWrapper {
            action,
            input,
            input_codec,
            return_codec,
            ..
        } => shell_activity(builder, action, *input, *input_codec, return_codec, &header),
        TemplateFn::ActivityWrapperRaw { .. } => {
            Err(SelectError::unsupported("T-ACTRAW shell", Span::zero()))
        }
        TemplateFn::SignalRef { .. } => Err(SelectError::unsupported("T-SIG shell", Span::zero())),
        TemplateFn::DeadBody => Err(SelectError::unsupported("standalone T-DEAD", Span::zero())),
        TemplateFn::ChildWitness => Err(SelectError::unsupported("T-WIT shell", Span::zero())),
    }
}

/// T-RUN: `make_fun2(execute)` + 2 codec calls + `call_ext_last run/4`.
fn shell_run(
    builder: &mut Builder<'_>,
    input_codec: FnRef,
    output_codec: &CodecRef,
    execute: FnRef,
    header: &Header,
) -> Result<Body, SelectError> {
    let mut shell = Shell::new(builder);
    let executor = shell.make_execute(execute)?;
    let input = shell.local_codec(input_codec);
    let output = shell.codec(output_codec)?;
    let tail = TailKind::TailImport {
        import: shell.builder.import(RuntimeFn::RtRun)?,
        arity: 4,
        args: vec![
            Src::Var(crate::mir::Var(0)),
            Src::Var(input),
            Src::Var(output),
            Src::Var(executor),
        ],
    };
    Ok(shell.finish(tail, header))
}

/// T-DEF: `make_fun2(execute)` + name binary + 3 codec calls + `call_ext_last
/// define/5` (S7).
fn shell_definition(
    builder: &mut Builder<'_>,
    workflow_name: &str,
    input_codec: FnRef,
    output_codec: &CodecRef,
    execute: FnRef,
    header: &Header,
) -> Result<Body, SelectError> {
    let name_lit = builder.binary_literal(workflow_name.as_bytes().to_vec());
    let mut shell = Shell::new(builder);
    let executor = shell.make_execute(execute)?;
    let input = shell.local_codec(input_codec);
    let output = shell.codec(output_codec)?;
    let error = shell.fresh();
    let error_step = shell.import_call(error, RuntimeFn::ErrCodec)?;
    shell.steps.push(error_step);
    let tail = TailKind::TailImport {
        import: shell.builder.import(RuntimeFn::WfDefine)?,
        arity: 5,
        args: vec![
            Src::Lit(name_lit),
            Src::Var(input),
            Src::Var(output),
            Src::Var(error),
            Src::Var(executor),
        ],
    };
    Ok(shell.finish(tail, header))
}

/// T-EXEC: `get_tuple_element` per input field, then tail-call the entry region.
fn shell_execute(
    builder: &mut Builder<'_>,
    field_count: usize,
    entry: FnRef,
    entry_args: &[u16],
    header: &Header,
) -> Result<Body, SelectError> {
    let mut shell = Shell::new(builder);
    let mut fields = Vec::with_capacity(field_count);
    for index in 0..field_count {
        let dst = shell.fresh();
        let element = u16::try_from(index + 1).map_err(|_| SelectError::OutOfRange {
            what: "input field index".to_owned(),
        })?;
        shell.steps.push(Step::FieldGet {
            dst,
            base: crate::mir::Var(0),
            index: element,
        });
        fields.push(dst);
    }
    let (entry_arity, entry_label) = entry_target(shell.builder.module, entry)?;
    let mut args = Vec::with_capacity(entry_args.len());
    for selector in entry_args {
        let field = fields
            .get(usize::from(*selector))
            .ok_or_else(|| SelectError::invariant("execute entry_arg selects an unknown field"))?;
        args.push(Src::Var(*field));
    }
    let call_arity = u8::try_from(args.len()).map_err(|_| SelectError::OutOfRange {
        what: "entry arity".to_owned(),
    })?;
    debug_assert_eq!(call_arity, entry_arity);
    let tail = TailKind::TailLocal {
        label: entry_label,
        arity: call_arity,
        args,
    };
    Ok(shell.finish(tail, header))
}

/// T-ACT: build the input record, 2 codec calls, `make_fun2(T-DEAD)`, then
/// `call_ext_last activity:new/5`.
fn shell_activity(
    builder: &mut Builder<'_>,
    action: &str,
    input: crate::mir::TypeShapeRef,
    input_codec: FnRef,
    return_codec: &CodecRef,
    header: &Header,
) -> Result<Body, SelectError> {
    let name_lit = builder.binary_literal(action.as_bytes().to_vec());
    let tag = record_tag(builder.module, input)?;
    let tag_atom = builder.mir_atom(tag)?;
    let mut shell = Shell::new(builder);
    let record = shell.fresh();
    let params = (0..header.param_count)
        .map(crate::mir::Var)
        .map(Src::Var)
        .collect();
    shell.steps.push(Step::Record {
        dst: record,
        tag: tag_atom,
        args: params,
    });
    let input_codec = shell.local_codec(input_codec);
    let return_codec = shell.codec(return_codec)?;
    let dead = shell.builder.request_dead();
    let dead_var = shell.fresh();
    shell.steps.push(Step::MakeClosure {
        dst: dead_var,
        lambda: dead,
        captures: Vec::new(),
    });
    let tail = TailKind::TailImport {
        import: shell.builder.import(RuntimeFn::ActNew)?,
        arity: 5,
        args: vec![
            Src::Lit(name_lit),
            Src::Var(record),
            Src::Var(input_codec),
            Src::Var(return_codec),
            Src::Var(dead_var),
        ],
    };
    Ok(shell.finish(tail, header))
}

fn entry_target(module: &MirModule, entry: FnRef) -> Result<(u8, u32), SelectError> {
    let function = module
        .function(entry)
        .ok_or_else(|| SelectError::invariant("execute entry region out of range"))?;
    let arity = u8::try_from(MirModule::arity(function)).map_err(|_| SelectError::OutOfRange {
        what: "entry arity".to_owned(),
    })?;
    Ok((arity, Builder::fn_labels(entry).body))
}

fn record_tag(module: &MirModule, shape: crate::mir::TypeShapeRef) -> Result<u32, SelectError> {
    match module.types.get(usize::from(shape.0)) {
        Some(TypeShape::Record { tag, .. }) => Ok(tag.0),
        _ => Err(SelectError::invariant(
            "activity input is not a record shape",
        )),
    }
}
