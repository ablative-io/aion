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

/// Whether a region must run each item as a synthesized child workflow —
/// crate-facing so the MIR direct path dispatches fan-out tracks and plans
/// child adapters from the exact same gate (D-BC1, zero drift).
pub(crate) fn implicit_child_required(emitter: &Emitter<'_>, region: &RegionShape) -> bool {
    is_required(emitter, region)
}

/// Every synthesized same-package workflow entry a document's parallel
/// multi-step regions require, in the emission order of the Gleam path
/// (`flows.rs::emit_nested`: regions by ascending id). Shared by both
/// backends so the two paths' manifests can never drift.
pub(crate) fn synthesized_entries(
    emitter: &Emitter<'_>,
    plans: &super::graph::Plans<'_>,
) -> Result<Vec<super::artifact::SynthesizedWorkflowEntry>, EmitError> {
    let mut entries = Vec::new();
    for (&id, nested) in &plans.regions {
        let Some(&region) = plans.region_shapes.get(&id) else {
            return Err(EmitError::new(
                emitter.document.span,
                format!("region {id} lost its shape"),
            ));
        };
        if !is_required(emitter, region) {
            continue;
        }
        let Some(bindings) = emitter.region_bindings.get(&id) else {
            return Err(EmitError::new(
                region.span,
                format!("region {id} has no binding environment"),
            ));
        };
        let Some(item_ty) = bindings.get(&region.binding) else {
            return Err(EmitError::new(
                region.span,
                format!(
                    "the collected binding `{}` has no established type",
                    region.binding
                ),
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
        entries.push(super::artifact::SynthesizedWorkflowEntry {
            workflow_type: region.child_name.clone(),
            entry_module: emitter.document.name.clone(),
            entry_function: entry_fn(region),
            input_schema: super::artifact::schema_for_fields(&emitter.env, &fields),
            output_schema: super::artifact::schema_for_type(&emitter.env, item_ty),
            timeout: document_timeout(emitter),
            internal: true,
        });
    }
    Ok(entries)
}

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
pub(crate) fn entry_fn(region: &RegionShape) -> String {
    format!("{}_run", snake(&region.child_name))
}

/// The parent document's declared workflow timeout, or `None` when the document
/// authored no `timeout`. A synthesized child inherits exactly this: when the
/// document declares no timeout there is no buried default — the child carries
/// `None` and the engine arms no deadline for it.
fn document_timeout(emitter: &Emitter<'_>) -> Option<std::time::Duration> {
    emitter
        .document
        .timeout
        .as_ref()
        .and_then(|timeout| timeout.duration.checked_duration())
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

    let child_timeout = document_timeout(emitter);
    emitter
        .synthesized_workflows
        .push(super::artifact::SynthesizedWorkflowEntry {
            workflow_type: region.child_name.clone(),
            entry_module: emitter.document.name.clone(),
            entry_function: entry_fn(region),
            input_schema: super::artifact::schema_for_fields(&emitter.env, &fields),
            output_schema: super::artifact::schema_for_type(&emitter.env, item_ty),
            timeout: child_timeout,
            internal: true,
        });

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
        "pub fn {entry}(raw_input: Dynamic) -> Result(String, String) {{"
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
) -> Result<(), EmitError> {
    let Some(item_ty) = emitter
        .region_bindings
        .get(&region.id)
        .and_then(|bindings| bindings.get(&region.binding))
        .cloned()
    else {
        return Err(EmitError::new(
            region.span,
            format!("implicit child region {} has no result type", region.id),
        ));
    };
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
    let output_codec = emitter.child_output_codec_fn(&item_ty);
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
