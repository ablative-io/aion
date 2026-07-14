//! Fixed T-WIT expansion for the string-name child-spawn type witness.

use crate::mir::Var;

use super::builder::Builder;
use super::ir::{Body, Src, Step, TailKind};
use super::shells::Header;

const MESSAGE: &[u8] = b"child workflow body runs in its own execution";

pub(super) fn lower(builder: &mut Builder<'_>, header: &Header) -> Body {
    let message = builder.binary_literal(MESSAGE.to_vec());
    let child_failed = builder.atom("awl_child_failed");
    let error = builder.atom("error");
    let failure = Var(header.param_count);
    let result = Var(header.param_count + 1);
    let steps = vec![
        Step::Record {
            dst: failure,
            tag: child_failed,
            args: vec![Src::Lit(message)],
        },
        Step::Record {
            dst: result,
            tag: error,
            args: vec![Src::Var(failure)],
        },
    ];
    Body {
        params: (0..header.param_count).map(Var).collect(),
        steps,
        tail: TailKind::Return(Src::Var(result)),
        name: header.name,
        module: header.module,
        arity: header.arity,
        entry_label: header.entry_label,
        code_label: header.code_label,
    }
}
