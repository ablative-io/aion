//// `aion_flow` is the typed Gleam SDK for authoring durable Aion
//// workflows.
////
//// Workflow authors write ordinary deterministic Gleam for decisions, loops,
//// and data transformation. Code reaches the outside world only through the
//// SDK primitives exposed from the public modules listed below: activities,
//// signals, queries, timers, child workflows, codecs, durations, errors, and
//// the pure-Gleam testing harness.
////
//// The recorded side-effect boundary is structural: activity dispatch goes
//// through `aion/workflow.run` and its typed concurrency helpers. Workflow code
//// must not read wall clocks or ambient entropy; use the deterministic
//// `aion/workflow.now`, `aion/workflow.random`, and timer primitives instead so
//// replay observes the same values.
////
//// Public import paths:
////
//// - `aion/activity` for typed activity definitions and configuration.
//// - `aion/workflow` for workflow definitions, deterministic primitives,
////   timers, child workflow helpers, and activity dispatch.
//// - `aion/signal` for typed signal references and signal helpers.
//// - `aion/query` for typed query handlers and replies.
//// - `aion/child` for typed child-workflow handles.
//// - `aion/error` for activity and engine-originated error types.
//// - `aion/codec` for payload codecs and decode errors.
//// - `aion/duration` for canonical workflow durations.
//// - `aion/testing` for simulated time, activity mocks, and replay assertions.
