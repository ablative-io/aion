//! Synthesized child-workflow adapters for parallel multi-step regions.

use std::collections::BTreeMap;

use crate::ast::DeliveryVerb;

use super::context::Emitter;
use super::error::EmitError;
use super::exprs::Scope;
use super::flowshape::RegionShape;
use super::graph::NestedPlan;
use super::names::{ident, snake, string_lit};
use super::stmts::CHILD_WITNESS;
use super::types::GType;

/// Whether a region must run each item as a synthesized child workflow.
pub(super) fn is_required(emitter: &Emitter<'_>, region: &RegionShape) -> bool {
    if !matches!(region.verb, DeliveryVerb::Distribute) {
        return false;
    }
    let Some(call) = super::flows::single_member_call(region) else {
        return true;
    };
    !emitter.actions.contains_key(call.call.name.as_str())
        && !emitter.children.contains_key(call.call.name.as_str())
}

/// Exported engine entry function for a region's synthesized workflow type.
pub(super) fn entry_fn(region: &RegionShape) -> String {
    format!("{}_run", snake(&region.child_name))
}

/// Emit the input record, codec, typed body, and raw engine adapter for one
/// implicit child workflow.
pub(super) fn emit_adapter(
    emitter: &mut Emitter<'_>,
    region: &RegionShape,
    nested: &NestedPlan,
    bindings: &BTreeMap<String, GType>,
    item_ty: &GType,
    instance_fn: &str,
) -> Result<(), EmitError> {
    let Some(input_type) = emitter.region_input_types.get(&region.id).cloned() else {
        return Err(EmitError::new(
            region.span,
            format!("region {} lost its implicit child input type", region.id),
        ));
    };
    let mut fields = Vec::with_capacity(nested.wrapper_params.len());
    for name in &nested.wrapper_params {
        let Some(ty) = bindings.get(name).cloned() else {
            return Err(EmitError::new(
                region.span,
                format!("implicit child parameter `{name}` has no established type"),
            ));
        };
        fields.push((name.clone(), ty));
    }

    super::frame::emit_record_type(emitter, &input_type, &fields);
    super::codecs::record_codec(emitter, &input_type, &fields);
    let execute = format!("{}_execute", snake(&region.child_name));
    let output = emitter.env.gleam_type(item_ty);
    emitter.line(&format!(
        "fn {execute}(input: {input_type}) -> Result({output}, awl_error.AwlError) {{"
    ));
    let args = nested
        .wrapper_params
        .iter()
        .map(|name| format!("input.{}", ident(name)))
        .collect::<Vec<_>>()
        .join(", ");
    emitter.indented(|this| this.line(&format!("{instance_fn}({args})")));
    emitter.line("}");
    emitter.blank();

    let entry = entry_fn(region);
    let input_codec = snake(&input_type);
    let output_codec = emitter.child_output_codec_fn(item_ty);
    emitter.line("/// Engine entry point for an implicit parallel region child.");
    emitter.line(&format!(
        "pub fn {entry}(raw_input: Dynamic) -> Result(String, awl_error.AwlError) {{"
    ));
    emitter.indented(|this| {
        this.line(&format!(
            "runtime.run(raw_input, {input_codec}_codec(), {output_codec}(), {execute})"
        ));
    });
    emitter.line("}");
    emitter.blank();

    let stem = emitter.env.codec_name(item_ty);
    emitter
        .implicit_child_outputs
        .entry(stem)
        .or_insert_with(|| item_ty.clone());
    Ok(())
}

/// Spawn every region item before awaiting handles in item order.
pub(super) fn emit_fanout(
    emitter: &mut Emitter<'_>,
    region: &RegionShape,
    nested: &NestedPlan,
    branch_scope: &Scope,
    items: &str,
    var: &str,
    bind: &str,
    item_ty: &GType,
) -> Result<(), EmitError> {
    let mut fields = Vec::with_capacity(nested.wrapper_params.len());
    for name in &nested.wrapper_params {
        let Some(ty) = branch_scope.get(name) else {
            return Err(EmitError::new(
                region.span,
                format!("implicit child parameter `{name}` is not in parent scope"),
            ));
        };
        fields.push(format!(
            "#({}, {}({}))",
            string_lit(name),
            emitter.to_json_fn(ty),
            ident(name)
        ));
    }
    let input = format!("json.object([{}])", fields.join(", "));
    let output_codec = emitter.child_output_codec_fn(item_ty);
    let spawn = format!(
        "({}, {CHILD_WITNESS}, {input}, awlc.json_value(), {output_codec}(), awl_error.codec())",
        string_lit(&region.child_name)
    );
    emitter.flags.uses_child_module = true;
    emitter.flags.uses_list_module = true;
    if region.tolerant {
        super::child_fanout::emit_tolerant(emitter, &spawn, items, var, bind);
    } else {
        super::child_fanout::emit_strict(emitter, &spawn, items, var, bind);
    }
    Ok(())
}
