//! The BC-2 coverage ratchet: the exact set of `valid/` fixtures the direct
//! path lowers (split from `tests` for the 500-line law).

/// The exact set of `valid/` fixtures this BC-2 increment lowers, pinned so a
/// regression from covered → refused (or a newly-covered fixture) fails the
/// ratchet instead of being silently absorbed by the deferred bucket. Paths
/// are relative to `tests/fixtures/rev2`, without the `.awl` extension.
pub(super) const COVERED: &[&str] = &[
    "dag-fork/valid/after_single",
    "dag-fork/valid/child_collection_fork",
    "dag-fork/valid/child_collection_fork_sequential",
    // BC-4 adversarial fixtures (rev-2): a fork over a possibly-empty input
    // collection (zero-item boundary) and a fork whose fan-out width is only
    // known at run time — both lower, so both join the ratchet and the
    // differential corpus.
    "dag-fork/valid/empty_fork_collection",
    "dag-fork/valid/fall_through_chain",
    "dag-fork/valid/fork_action_fanout",
    "dag-fork/valid/fork_collection_join",
    "dag-fork/valid/fork_named_branches",
    "dag-fork/valid/fork_named_homogeneous",
    "dag-fork/valid/fork_sequential",
    "dag-fork/valid/fork_sequential_route",
    // BC-4 adversarial: a runtime-sized fork over a plain input list.
    "dag-fork/valid/runtime_sized_fork",
    "dag-fork/valid/sit_one",
    "declarations/valid/call_site_override",
    "declarations/valid/child_call_awaited",
    // BC-4 adversarial: await a child, then fire-and-forget a detached child.
    "declarations/valid/child_spawn_combo",
    "declarations/valid/declarations_combined",
    "declarations/valid/spawn_detached",
    // BC-4 adversarial: a per-attempt timeout nested inside a retry schedule.
    "declarations/valid/timeout_inside_retry",
    "declarations/valid/worker_action_config_lines",
    "declarations/valid/worker_retry_backoff",
    "declarations/valid/worker_single_action",
    "declarations/valid/workers_multiple",
    "ergonomics/valid/flow_vocab_b1",
    "flagship/valid/awl_hello",
    "flow-shape/valid/distribute_activity_tolerant",
    "flow-shape/valid/distribute_child_collect",
    "flow-shape/valid/distribute_child_tolerant",
    "flow-shape/valid/region_pure_decision",
    "flow-shape/valid/sequence_activity_tolerant",
    "flow-shape/valid/sequence_region_loopback",
    "header-types/valid/builtins",
    "header-types/valid/combined",
    "header-types/valid/doc_comments",
    "header-types/valid/enum",
    "header-types/valid/line_width",
    // BC-4 adversarial: a high-arity record — a wide tuple through the codec.
    "header-types/valid/max_arity_record",
    "header-types/valid/minimal",
    "header-types/valid/noncanonical_commas",
    "header-types/valid/signal_wait",
    "header-types/valid/workflow_timeout",
    "header-types/valid/zero_inputs",
    // `backward_route_bounded_cycle` regressed covered → refused at rev 3
    // (the cycle rule demanded a `max … visits` bound); the fan-out parity
    // landing lowers the bound, restoring coverage.
    "loop-outcomes/valid/backward_route_bounded_cycle",
    "loop-outcomes/valid/enum_when_totality",
    "loop-outcomes/valid/float_threshold_guard",
    "loop-outcomes/valid/fork_in_loop_live_ins",
    "loop-outcomes/valid/guard_optional_wait",
    "loop-outcomes/valid/loop_after_fall_through",
    "loop-outcomes/valid/loop_compound_until_nested",
    "loop-outcomes/valid/loop_counting_until_max",
    "loop-outcomes/valid/loop_without_counting",
    "loop-outcomes/valid/route_outcome_by_name",
    "schema-doors/valid/import_constraints",
    "schema-doors/valid/import_nested_defs",
    "schema-doors/valid/import_ticket",
    "schema-doors/valid/inline_schema_round",
    "schema-doors/valid/inline_verbatim_constraints",
    "schema-doors/valid/mixed_doors",
    "schema-doors/valid/optional_shorthand",
    "schema-doors/valid/short_circuit_optional",
    "step-bodies/valid/calls_and_side_effects",
    "step-bodies/valid/collection_predicates",
    "step-bodies/valid/combinators",
    "step-bodies/valid/fallible_all_short_circuit",
    "step-bodies/valid/fallible_any_short_circuit",
    "step-bodies/valid/fallible_collection_predicates",
    "step-bodies/valid/general_concat",
    "step-bodies/valid/index_and_concat",
    "step-bodies/valid/literal_forms",
    "step-bodies/valid/pipe_chain_stages",
    "step-bodies/valid/predicates_and_operators",
    "step-bodies/valid/step_bodies_combined",
    // BC-4 adversarial: UTF-8 string literals and concat through the wire.
    "step-bodies/valid/unicode_payloads",
    "step-bodies/valid/wait_and_sleep",
    "step-bodies/valid/wait_timeout_optional",
    "step-bodies/valid/workflow_id",
];
