use super::context::Emitter;
use super::helpers::{codec_name, constructor, gleam_type, ident, pascal, snake};

impl Emitter<'_> {
    pub(super) fn activity_wrappers(&mut self) {
        for action in &self.document.actions {
            let action_type_name = pascal(&action.name);
            let input_name = format!("{action_type_name}Input");
            self.emit_type(
                &input_name,
                action
                    .params
                    .iter()
                    .map(|field| (field.name.as_str(), &field.ty)),
            );
            let action_name = &action.name;
            let return_type = gleam_type(&action.returns);
            if action.params.is_empty() {
                self.line(&format!(
                    "fn {action_name}_activity() -> activity.Activity({input_name}, {return_type}) {{"
                ));
            } else {
                self.line(&format!("fn {action_name}_activity("));
                self.indented(|this| {
                    for param in &action.params {
                        let name = ident(&param.name);
                        let ty = gleam_type(&param.ty);
                        this.line(&format!("{name}: {ty},"));
                    }
                });
                self.line(&format!(
                    ") -> activity.Activity({input_name}, {return_type}) {{"
                ));
            }
            self.indented(|this| {
                this.line("activity.new(");
                this.indented(|this| {
                    let action_name = &action.name;
                    let ctor = constructor(&input_name);
                    this.line(&format!("\"{action_name}\","));
                    if action.params.is_empty() {
                        this.line(&format!("{ctor},"));
                    } else {
                        this.line(&format!("{ctor}("));
                        this.indented(|this| {
                            for param in &action.params {
                                let name = ident(&param.name);
                                this.line(&format!("{name}: {name},"));
                            }
                        });
                        this.line("),");
                    }
                    let input_codec = snake(&input_name);
                    let return_codec = codec_name(&action.returns);
                    this.line(&format!("{input_codec}_codec(),"));
                    this.line(&format!("{return_codec}_codec(),"));
                    this.line("fn(_) { Error(error.terminal(\"activity body is provided by a worker\")) },");
                });
                this.line(")");
            });
            self.line("}");
            self.blank();
        }
    }

    pub(super) fn signal_refs(&mut self) {
        for signal_decl in &self.document.signals {
            let signal_name = &signal_decl.name;
            let signal_type = gleam_type(&signal_decl.ty);
            self.line(&format!(
                "fn {signal_name}_signal() -> signal.SignalRef({signal_type}) {{"
            ));
            self.indented(|this| {
                let codec = codec_name(&signal_decl.ty);
                this.line(&format!("signal.new(\"{signal_name}\", {codec}_codec())"));
            });
            self.line("}");
            self.blank();
        }
    }

    pub(super) fn error_mappers(&mut self) {
        self.line("fn try(result: Result(a, AwlError), next: fn(a) -> Result(b, AwlError)) -> Result(b, AwlError) {");
        self.indented(|this| {
            this.line(
                "case result { Ok(value) -> next(value) Error(awl_error) -> Error(awl_error) }",
            );
        });
        self.line("}");
        self.blank();
        self.line("fn map_activity_error(result: Result(a, error.ActivityError)) -> Result(a, AwlError) {");
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlActivityFailed(\"activity failed\")) }");
        });
        self.line("}");
        self.blank();
        self.line(
            "fn map_receive_error(result: Result(a, error.ReceiveError)) -> Result(a, AwlError) {",
        );
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlSignalFailed(\"signal receive failed\")) }");
        });
        self.line("}");
        self.blank();
        self.line("fn map_child_error(result: Result(a, error.ChildError(AwlError))) -> Result(a, AwlError) {");
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlChildFailed(\"child workflow failed\")) }");
        });
        self.line("}");
        self.blank();
        self.line(
            "fn map_timer_error(result: Result(a, error.EngineError)) -> Result(a, AwlError) {",
        );
        self.indented(|this| {
            this.line("case result { Ok(value) -> Ok(value) Error(_) -> Error(AwlTimerFailed(\"timer failed\")) }");
        });
        self.line("}");
        self.blank();
    }

    pub(super) fn child_helpers(&mut self) {
        if self.uses_child_calls {
            self.line("/// Encode-only codec for child workflow inputs: the parent assembles the");
            self.line("/// child's input record as JSON and never decodes it back.");
            self.line("fn json_value_codec() -> Codec(json.Json) {");
            self.indented(|this| {
                this.line("codec.Codec(");
                this.indented(|this| {
                    this.line("encode: json.to_string,");
                    this.line("decode: fn(_) {");
                    this.indented(|this| {
                        this.line("Error(codec.DecodeError(reason: \"child call input is encode-only\", path: []))");
                    });
                    this.line("},");
                });
                this.line(")");
            });
            self.line("}");
            self.blank();
        }
        if self.uses_child_retry {
            self.line("/// Bounded retry with backoff for child workflow calls: `attempts` total");
            self.line("/// attempts with an SDK timer sleep between them (fixed when `multiplier`");
            self.line("/// is 1, exponential capped at `max_delay_ms` otherwise).");
            self.line("fn awl_retry(");
            self.indented(|this| {
                this.line("attempts: Int,");
                this.line("delay_ms: Int,");
                this.line("multiplier: Int,");
                this.line("max_delay_ms: Int,");
                this.line("operation: fn() -> Result(value, AwlError),");
            });
            self.line(") -> Result(value, AwlError) {");
            self.indented(|this| {
                this.line("case operation() {");
                this.indented(|this| {
                    this.line("Ok(value) -> Ok(value)");
                    this.line("Error(awl_error) ->");
                    this.indented(|this| {
                        this.line("case attempts <= 1 {");
                        this.indented(|this| {
                            this.line("True -> Error(awl_error)");
                            this.line("False ->");
                            this.indented(|this| {
                                this.line("case workflow.sleep(duration.milliseconds(delay_ms)) {");
                                this.indented(|this| {
                                    this.line("Ok(_) -> {");
                                    this.indented(|this| {
                                        this.line("let next_delay_ms = delay_ms * multiplier");
                                        this.line("let capped_delay_ms = case next_delay_ms > max_delay_ms {");
                                        this.indented(|this| {
                                            this.line("True -> max_delay_ms");
                                            this.line("False -> next_delay_ms");
                                        });
                                        this.line("}");
                                        this.line("awl_retry(attempts - 1, capped_delay_ms, multiplier, max_delay_ms, operation)");
                                    });
                                    this.line("}");
                                    this.line("Error(_) -> Error(AwlTimerFailed(\"timer failed while backing off\"))");
                                });
                                this.line("}");
                            });
                        });
                        this.line("}");
                    });
                });
                this.line("}");
            });
            self.line("}");
            self.blank();
        }
    }
}
