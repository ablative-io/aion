//! Activity wrappers (one per declared action, task queue baked in at the
//! call site), signal references, error mappers, and the child-spawn and
//! indexing helpers.

use super::context::Emitter;
use super::frame::emit_record_type;
use super::names::{ident, snake};
use super::types::type_ref_to_g;

pub(super) fn activity_wrappers(emitter: &mut Emitter<'_>) {
    let workers: Vec<usize> = (0..emitter.document.workers.len()).collect();
    for worker_index in workers {
        let action_count = emitter.document.workers[worker_index].actions.len();
        for action_index in 0..action_count {
            let action = &emitter.document.workers[worker_index].actions[action_index];
            let action_name = action.name.clone();
            let docs: Vec<String> = action
                .docs
                .iter()
                .map(|line| line.text.strip_prefix(' ').unwrap_or(&line.text).to_owned())
                .collect();
            let params: Vec<(String, super::types::GType)> = action
                .params
                .iter()
                .map(|param| (param.name.clone(), type_ref_to_g(&param.ty)))
                .collect();
            let returns = type_ref_to_g(&action.returns);
            let input_name = emitter
                .action_inputs
                .get(&action_name)
                .cloned()
                .unwrap_or_else(|| format!("{}Input", super::names::pascal(&action_name)));

            emit_record_type(emitter, &input_name, &params);

            for doc in &docs {
                emitter.line(&format!("/// {doc}"));
            }
            let return_type = emitter.env.gleam_type(&returns);
            let wrapper = format!("{}_activity", snake(&action_name));
            if params.is_empty() {
                emitter.line(&format!(
                    "fn {wrapper}() -> activity.Activity({input_name}, {return_type}) {{"
                ));
            } else {
                emitter.line(&format!("fn {wrapper}("));
                emitter.indented(|this| {
                    for (name, ty) in &params {
                        let rendered = this.env.gleam_type(ty);
                        this.line(&format!("{}: {rendered},", ident(name)));
                    }
                });
                emitter.line(&format!(
                    ") -> activity.Activity({input_name}, {return_type}) {{"
                ));
            }
            emitter.indented(|this| {
                this.line("activity.new(");
                this.indented(|this| {
                    this.line(&format!("\"{action_name}\","));
                    if params.is_empty() {
                        this.line(&format!("{input_name},"));
                    } else {
                        this.line(&format!("{input_name}("));
                        this.indented(|this| {
                            for (name, _) in &params {
                                let rendered = ident(name);
                                this.line(&format!("{rendered}: {rendered},"));
                            }
                        });
                        this.line("),");
                    }
                    let input_codec = snake(&input_name);
                    let return_codec = this.env.codec_name(&returns);
                    this.line(&format!("{input_codec}_codec(),"));
                    this.line(&format!("{return_codec}_codec(),"));
                    this.line(
                        "fn(_) { Error(error.terminal(\"activity body is provided by a \
                         worker\")) },",
                    );
                });
                this.line(")");
            });
            emitter.line("}");
            emitter.blank();
        }
    }
}

pub(super) fn signal_refs(emitter: &mut Emitter<'_>) {
    for signal_index in 0..emitter.document.signals.len() {
        let signal = &emitter.document.signals[signal_index];
        let signal_name = signal.name.clone();
        let payload = type_ref_to_g(&signal.ty);
        let signal_type = emitter.env.gleam_type(&payload);
        let codec = emitter.env.codec_name(&payload);
        emitter.line(&format!(
            "fn {}_signal() -> signal.SignalRef({signal_type}) {{",
            snake(&signal_name)
        ));
        emitter.indented(|this| {
            this.line(&format!("signal.new(\"{signal_name}\", {codec}_codec())"));
        });
        emitter.line("}");
        emitter.blank();
    }
}

/// The `try` chain helper, the error mappers, literal indexing, and the
/// encode-only child-input codec.
pub(super) fn helpers(emitter: &mut Emitter<'_>) {
    error_mappers(emitter);
    emitter.line("/// Literal-only list indexing; out of range is a step failure.");
    emitter
        .line("fn awl_index(items: List(a), index: Int, label: String) -> Result(a, AwlError) {");
    emitter.indented(|this| {
        this.line("case list.drop(items, index) |> list.first {");
        this.indented(|this| {
            this.line("Ok(value) -> Ok(value)");
            this.line("Error(_) -> Error(AwlIndexOutOfRange(label))");
        });
        this.line("}");
    });
    emitter.line("}");
    emitter.blank();
    if emitter.flags.uses_child {
        emitter.line("/// Encode-only codec for child workflow inputs: the parent assembles the");
        emitter.line("/// child's input record as JSON and never decodes it back.");
        emitter.line("fn json_value_codec() -> Codec(json.Json) {");
        emitter.indented(|this| {
            this.line("codec.Codec(");
            this.indented(|this| {
                this.line("encode: json.to_string,");
                this.line("decode: fn(_) {");
                this.indented(|this| {
                    this.line(
                        "Error(codec.DecodeError(reason: \"child call input is encode-only\", \
                         path: []))",
                    );
                });
                this.line("},");
            });
            this.line(")");
        });
        emitter.line("}");
        emitter.blank();
    }
}

fn error_mappers(emitter: &mut Emitter<'_>) {
    emitter.line(
        "fn try(result: Result(a, AwlError), next: fn(a) -> Result(b, AwlError)) -> Result(b, \
         AwlError) {",
    );
    emitter.indented(|this| {
        this.line("case result { Ok(value) -> next(value) Error(awl_error) -> Error(awl_error) }");
    });
    emitter.line("}");
    emitter.blank();
    emitter.line(
        "fn map_activity_error(result: Result(a, error.ActivityError)) -> Result(a, AwlError) {",
    );
    emitter.indented(|this| {
        this.line(
            "case result { Ok(value) -> Ok(value) Error(_) -> \
             Error(AwlActivityFailed(\"activity failed\")) }",
        );
    });
    emitter.line("}");
    emitter.blank();
    emitter.line(
        "fn map_receive_error(result: Result(a, error.ReceiveError)) -> Result(a, AwlError) {",
    );
    emitter.indented(|this| {
        this.line(
            "case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlSignalFailed(\"signal \
             receive failed\")) }",
        );
    });
    emitter.line("}");
    emitter.blank();
    emitter.line(
        "fn map_child_error(result: Result(a, error.ChildError(AwlError))) -> Result(a, \
         AwlError) {",
    );
    emitter.indented(|this| {
        this.line(
            "case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlChildFailed(\"child \
             workflow failed\")) }",
        );
    });
    emitter.line("}");
    emitter.blank();
    emitter
        .line("fn map_spawn_error(result: Result(a, error.EngineError)) -> Result(a, AwlError) {");
    emitter.indented(|this| {
        this.line(
            "case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlChildFailed(\"detached \
             spawn failed\")) }",
        );
    });
    emitter.line("}");
    emitter.blank();
    emitter
        .line("fn map_timer_error(result: Result(a, error.EngineError)) -> Result(a, AwlError) {");
    emitter.indented(|this| {
        this.line(
            "case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlTimerFailed(\"timer \
             failed\")) }",
        );
    });
    emitter.line("}");
    emitter.blank();
}
