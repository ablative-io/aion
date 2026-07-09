use crate::TypeRef;

use super::context::Emitter;
use super::helpers::{
    codec_name, collect_composite_type, constructor, gleam_type, ident, pascal, snake,
};

impl Emitter<'_> {
    pub(super) fn codecs(&mut self) {
        self.line("fn awl_error_codec() -> Codec(AwlError) {");
        self.indented(|this| {
            this.line("codec.json_codec(awl_error_to_json, awl_error_decoder())");
        });
        self.line("}");
        self.blank();
        self.line("fn awl_error_to_json(error_value: AwlError) -> json.Json {");
        self.indented(|this| {
            this.line("case error_value {");
            this.indented(|this| {
                for variant in ["AwlDecodeInputFailed", "AwlActivityFailed", "AwlSignalFailed", "AwlChildFailed", "AwlTimerFailed", "AwlTimedOut"] {
                    this.line(&format!("{variant}(message) -> json.object([#(\"tag\", json.string(\"{variant}\")), #(\"message\", json.string(message))])"));
                }
                this.line("AwlFailed -> json.object([#(\"tag\", json.string(\"AwlFailed\"))])");
            });
            this.line("}");
        });
        self.line("}");
        self.blank();
        self.line("fn awl_error_decoder() -> decode.Decoder(AwlError) {");
        self.indented(|this| {
            this.line("use tag <- decode.field(\"tag\", decode.string)");
            this.line("case tag {");
            this.indented(|this| {
                for variant in [
                    "AwlDecodeInputFailed",
                    "AwlActivityFailed",
                    "AwlSignalFailed",
                    "AwlChildFailed",
                    "AwlTimerFailed",
                    "AwlTimedOut",
                ] {
                    this.line(&format!("\"{variant}\" -> {{"));
                    this.indented(|this| {
                        this.line("use message <- decode.field(\"message\", decode.string)");
                        this.line(&format!("decode.success({variant}(message))"));
                    });
                    this.line("}");
                }
                this.line("\"AwlFailed\" -> decode.success(AwlFailed)");
                this.line("_ -> decode.failure(AwlFailed, \"AwlError\")");
            });
            this.line("}");
        });
        self.line("}");
        self.blank();

        for name in self.external_named_types() {
            self.emit_external_codec(&name);
        }
        self.emit_codec(
            &self.input_type_name(),
            self.document
                .inputs
                .iter()
                .map(|field| (field.name.as_str(), &field.ty)),
        );
        for decl in &self.document.types {
            self.emit_codec(
                &decl.name,
                decl.fields
                    .iter()
                    .map(|field| (field.name.as_str(), &field.ty)),
            );
        }
        for action in &self.document.actions {
            let action_type_name = pascal(&action.name);
            self.emit_codec(
                &format!("{action_type_name}Input"),
                action
                    .params
                    .iter()
                    .map(|field| (field.name.as_str(), &field.ty)),
            );
        }
        let mut composite_types = Vec::new();
        self.collect_composite_types(&mut composite_types);
        for ty in composite_types {
            self.emit_composite_codec(&ty);
        }
        self.builtin_codecs();
        self.error_mappers();
        self.child_helpers();
    }

    fn emit_external_codec(&mut self, name: &str) {
        let codec_fn = snake(name);
        let constructor_name = constructor(name);
        self.line(&format!("fn {codec_fn}_codec() -> Codec({name}) {{"));
        self.indented(|this| {
            this.line(&format!(
                "codec.json_codec({codec_fn}_to_json, {codec_fn}_decoder())"
            ));
        });
        self.line("}");
        self.line(&format!(
            "fn {codec_fn}_to_json(value: {name}) -> json.Json {{"
        ));
        self.indented(|this| {
            this.line("json.string(value.value)");
        });
        self.line("}");
        self.line(&format!(
            "fn {codec_fn}_decoder() -> decode.Decoder({name}) {{"
        ));
        self.indented(|this| {
            this.line("use value <- decode.then(decode.string)");
            this.line(&format!("decode.success({constructor_name}(value: value))"));
        });
        self.line("}");
        self.blank();
    }

    fn emit_codec<'b, I>(&mut self, name: &str, fields: I)
    where
        I: IntoIterator<Item = (&'b str, &'b TypeRef)>,
    {
        let fields = fields.into_iter().collect::<Vec<_>>();
        let codec_fn = snake(name);
        let value_var = ident(&snake(name));
        self.line(&format!("fn {codec_fn}_codec() -> Codec({name}) {{"));
        self.indented(|this| {
            this.line(&format!(
                "codec.json_codec({codec_fn}_to_json, {codec_fn}_decoder())"
            ));
        });
        self.line("}");
        self.blank();
        if fields.is_empty() {
            self.line(&format!("fn {codec_fn}_to_json(_: {name}) -> json.Json {{"));
        } else {
            self.line(&format!(
                "fn {codec_fn}_to_json({value_var}: {name}) -> json.Json {{"
            ));
        }
        self.indented(|this| {
            this.line("json.object([");
            this.indented(|this| {
                for (field_name, field_type) in &fields {
                    let codec = codec_name(field_type);
                    let access = ident(field_name);
                    this.line(&format!(
                        "#(\"{field_name}\", {codec}_to_json({value_var}.{access})),"
                    ));
                }
            });
            this.line("])");
        });
        self.line("}");
        self.blank();
        self.line(&format!(
            "fn {codec_fn}_decoder() -> decode.Decoder({name}) {{"
        ));
        self.indented(|this| {
            for (field_name, field_type) in &fields {
                let codec = codec_name(field_type);
                let binding = ident(field_name);
                this.line(&format!(
                    "use {binding} <- decode.field(\"{field_name}\", {codec}_decoder())"
                ));
            }
            let ctor = constructor(name);
            if fields.is_empty() {
                this.line(&format!("decode.success({ctor})"));
            } else {
                this.line(&format!("decode.success({ctor}("));
                this.indented(|this| {
                    for (field_name, _) in &fields {
                        let binding = ident(field_name);
                        this.line(&format!("{binding}: {binding},"));
                    }
                });
                this.line("))");
            }
        });
        self.line("}");
        self.blank();
    }

    fn collect_composite_types(&self, types: &mut Vec<TypeRef>) {
        for input in &self.document.inputs {
            collect_composite_type(&input.ty, types);
        }
        if let Some(output) = &self.document.output {
            collect_composite_type(&output.ty, types);
        }
        for signal_decl in &self.document.signals {
            collect_composite_type(&signal_decl.ty, types);
        }
        for decl in &self.document.types {
            for field in &decl.fields {
                collect_composite_type(&field.ty, types);
            }
        }
        for action in &self.document.actions {
            for param in &action.params {
                collect_composite_type(&param.ty, types);
            }
            collect_composite_type(&action.returns, types);
        }
    }

    fn emit_composite_codec(&mut self, ty: &TypeRef) {
        match ty {
            TypeRef::Named { .. } => {}
            TypeRef::List { inner, .. } => {
                let name = codec_name(ty);
                let inner_name = codec_name(inner);
                self.line(&format!(
                    "fn {name}_codec() -> Codec({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!(
                        "codec.json_codec({name}_to_json, {name}_decoder())"
                    ));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_to_json(values: {}) -> json.Json {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("list_to_json(values, {inner_name}_to_json)"));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_decoder() -> decode.Decoder({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("list_decoder({inner_name}_decoder())"));
                });
                self.line("}");
                self.blank();
            }
            TypeRef::Option { inner, .. } => {
                let name = codec_name(ty);
                let inner_name = codec_name(inner);
                self.line(&format!(
                    "fn {name}_codec() -> Codec({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!(
                        "codec.json_codec({name}_to_json, {name}_decoder())"
                    ));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_to_json(value: {}) -> json.Json {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("option_to_json(value, {inner_name}_to_json)"));
                });
                self.line("}");
                self.line(&format!(
                    "fn {name}_decoder() -> decode.Decoder({}) {{",
                    gleam_type(ty)
                ));
                self.indented(|this| {
                    this.line(&format!("option_decoder({inner_name}_decoder())"));
                });
                self.line("}");
                self.blank();
            }
        }
    }

    pub(super) fn builtin_codecs(&mut self) {
        self.line(
            "fn string_codec() -> Codec(String) { codec.json_codec(json.string, decode.string) }",
        );
        self.line("fn int_codec() -> Codec(Int) { codec.json_codec(json.int, decode.int) }");
        self.line(
            "fn float_codec() -> Codec(Float) { codec.json_codec(json.float, decode.float) }",
        );
        self.line("fn bool_codec() -> Codec(Bool) { codec.json_codec(json.bool, decode.bool) }");
        self.line("fn nil_codec() -> Codec(Nil) { codec.json_codec(fn(_) { json.object([]) }, decode.success(Nil)) }");
        self.blank();
        self.line("fn string_to_json(value: String) -> json.Json { json.string(value) }");
        self.line("fn int_to_json(value: Int) -> json.Json { json.int(value) }");
        self.line("fn float_to_json(value: Float) -> json.Json { json.float(value) }");
        self.line("fn bool_to_json(value: Bool) -> json.Json { json.bool(value) }");
        self.line("fn nil_to_json(_: Nil) -> json.Json { json.object([]) }");
        self.blank();
        self.line("fn string_decoder() -> decode.Decoder(String) { decode.string }");
        self.line("fn int_decoder() -> decode.Decoder(Int) { decode.int }");
        self.line("fn float_decoder() -> decode.Decoder(Float) { decode.float }");
        self.line("fn bool_decoder() -> decode.Decoder(Bool) { decode.bool }");
        self.line("fn nil_decoder() -> decode.Decoder(Nil) { decode.success(Nil) }");
        self.blank();
        self.line("fn list_to_json(values: List(a), item_to_json: fn(a) -> json.Json) -> json.Json { json.array(values, item_to_json) }");
        self.line("fn list_decoder(item_decoder: decode.Decoder(a)) -> decode.Decoder(List(a)) { decode.list(item_decoder) }");
        self.line("fn option_to_json(value: Option(a), item_to_json: fn(a) -> json.Json) -> json.Json { json.nullable(value, item_to_json) }");
        self.line("fn option_decoder(item_decoder: decode.Decoder(a)) -> decode.Decoder(Option(a)) { decode.optional(item_decoder) }");
        self.blank();
    }
}
