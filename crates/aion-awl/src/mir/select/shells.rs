//! Template-shell expansion (AWL-BC-IR.md §2.4 / §11.4 "Shell expansion — one
//! selector"): each `TemplateFn` expands into a resolved [`Body`] in the same
//! op vocabulary flow functions use, so a single emitter owns every function.
//! Recipes are name-substitution-only and mint at most one closure (S8); T-ACT
//! additionally references the shared dead-body lambda (T-DEAD), whose body is a
//! real `TemplateFn::DeadBody` function in `module.functions` (golden- and
//! sidecar-visible, S8 "select never synthesizes a function"), expanded here.

use crate::mir::runtime::RuntimeFn;
use crate::mir::{CodecRef, FnOrigin, FnRef, MirModule, Span, TemplateFn, TypeShape};

use super::builder::Builder;
use super::error::SelectError;
use super::ir::{Body, Src, Step, TailKind};

/// The message the dead activity body raises (matches the reference emitter's
/// `wrappers.rs` `fn(_) { Error(error.terminal(...)) }`).
const DEAD_MESSAGE: &[u8] = b"activity body is provided by a worker";

/// The synthetic name of the shared dead-body function (`$` marks generated
/// glue); `lower` mints one `TemplateFn::DeadBody` under this name per module
/// that has any activity, so T-ACT's `make_fun2` and the FunT/sidecar all refer
/// to a real function.
const DEAD_NAME: &str = "awl$dead_body";

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

/// The `FnRef` of the module's shared dead-body function (`TemplateFn::DeadBody`,
/// minted by `lower` whenever the module has an activity). Every T-ACT wrapper
/// closes over it.
fn find_dead(module: &MirModule) -> Result<FnRef, SelectError> {
    module
        .functions
        .iter()
        .position(|function| matches!(function.origin(), FnOrigin::DeadBody))
        .map(|index| FnRef(u32::try_from(index).unwrap_or(u32::MAX)))
        .ok_or_else(|| SelectError::invariant("activity wrapper but no dead-body function"))
}

struct Shell<'b, 'm> {
    builder: &'b mut Builder<'m>,
    next_var: u32,
    steps: Vec<Step>,
}

impl<'b, 'm> Shell<'b, 'm> {
    /// `param_count` seeds the fresh-var counter ABOVE the shell's parameters:
    /// params are `Var(0..param_count)` (see [`Shell::finish`]) and share their
    /// Y home with the same-numbered var (`FramePlan`), so a fresh temp that
    /// aliased `Var(0)` would clobber the raw-input parameter (T-RUN payload,
    /// T-EXEC input tuple). Fresh temps therefore start at `param_count`.
    fn new(builder: &'b mut Builder<'m>, param_count: u32) -> Self {
        Self {
            builder,
            next_var: param_count,
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
        TemplateFn::ActivityWrapperRaw {
            action,
            input,
            input_codec,
            ..
        } => shell_activity_raw(builder, action, *input, *input_codec, &header),
        TemplateFn::SignalRef { .. } => Err(SelectError::unsupported("T-SIG shell", Span::zero())),
        TemplateFn::DeadBody => shell_dead(builder, &header),
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
    let mut shell = Shell::new(builder, header.param_count);
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
    let mut shell = Shell::new(builder, header.param_count);
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
    let mut shell = Shell::new(builder, header.param_count);
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
    let dead_ref = find_dead(builder.module)?;
    let mut shell = Shell::new(builder, header.param_count);
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
    let dead_name = shell.builder.atom(DEAD_NAME);
    let dead_label = Builder::fn_labels(dead_ref).body;
    let dead = shell.builder.lambda(dead_name, 1, dead_label, 0);
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

/// T-ACTRAW: the raw twin of T-ACT (`emitter/wrappers.rs::raw_wrapper`): the
/// same action name and the same wire bytes — the input record is encoded
/// with the action's own input codec (`codec.encode(record)`, one `call_fun`
/// of the codec's encode field) — but typed `Activity(String, String)` via
/// `awlc.raw()` twice, so differently-typed parallel branches share one
/// `workflow.all` list.
fn shell_activity_raw(
    builder: &mut Builder<'_>,
    action: &str,
    input: crate::mir::TypeShapeRef,
    input_codec: FnRef,
    header: &Header,
) -> Result<Body, SelectError> {
    let name_lit = builder.binary_literal(action.as_bytes().to_vec());
    let tag = record_tag(builder.module, input)?;
    let tag_atom = builder.mir_atom(tag)?;
    let dead_ref = find_dead(builder.module)?;
    let mut shell = Shell::new(builder, header.param_count);
    let codec = shell.local_codec(input_codec);
    // `Codec(encode, decode)`: the encode fn sits at element 1 (tag at 0).
    let encode = shell.fresh();
    shell.steps.push(Step::FieldGet {
        dst: encode,
        base: codec,
        index: 1,
    });
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
    let encoded = shell.fresh();
    shell.steps.push(Step::CallFun {
        dst: Some(encoded),
        fun: Src::Var(encode),
        args: vec![Src::Var(record)],
    });
    let raw_input = shell.fresh();
    let raw_input_step = shell.import_call(raw_input, RuntimeFn::RawCodec)?;
    shell.steps.push(raw_input_step);
    let raw_return = shell.fresh();
    let raw_return_step = shell.import_call(raw_return, RuntimeFn::RawCodec)?;
    shell.steps.push(raw_return_step);
    let dead_name = shell.builder.atom(DEAD_NAME);
    let dead_label = Builder::fn_labels(dead_ref).body;
    let dead = shell.builder.lambda(dead_name, 1, dead_label, 0);
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
            Src::Var(encoded),
            Src::Var(raw_input),
            Src::Var(raw_return),
            Src::Var(dead_var),
        ],
    };
    Ok(shell.finish(tail, header))
}

/// T-DEAD: the shared dead-body function `fn(_) -> Error(error.terminal(msg))`
/// (§2.4). One non-tail `error:terminal/1`, an `{error, _}` tuple, `return` — a
/// real `module.functions` entry (S8) that every T-ACT closes over.
fn shell_dead(builder: &mut Builder<'_>, header: &Header) -> Result<Body, SelectError> {
    let message = builder.binary_literal(DEAD_MESSAGE.to_vec());
    let error_atom = builder.atom("error");
    let terminal = builder.import(RuntimeFn::ErrorTerminal)?;
    let mut shell = Shell::new(builder, header.param_count);
    let result = shell.fresh();
    shell.steps.push(Step::CallImport {
        dst: Some(result),
        import: terminal,
        arity: 1,
        args: vec![Src::Lit(message)],
    });
    let tuple = shell.fresh();
    shell.steps.push(Step::Record {
        dst: tuple,
        tag: error_atom,
        args: vec![Src::Var(result)],
    });
    Ok(shell.finish(TailKind::Return(Src::Var(tuple)), header))
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
