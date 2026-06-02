# Aion-Flow — User Stories

## Workflow Author — Writing Durable Workflows in Gleam

**S1.** As a workflow author, I want activity inputs and outputs to be statically typed so that passing the wrong value to an activity is a compile error, not a runtime failure three days into a long-running workflow.

**S2.** As a workflow author, I want a single recorded way to invoke a side effect (workflow.run over an activity) and plain Gleam everywhere else, so that the recorded/deterministic boundary is obvious from the code's shape.

**S3.** As a workflow author, I want workflow.now and workflow.random to be deterministic and to have no wall-clock alternative available, so that my workflow cannot accidentally become non-deterministic and desynchronise replay.

**S4.** As a workflow author, I want to classify an activity failure as retryable or terminal once, in the type system, so that the engine applies my retry policy correctly without me re-stating the intent.

**S5.** As a workflow author, I want to compose retry, timeout, and heartbeat onto an activity in a readable pipeline so that the activity's policy reads as data next to its definition.

**S6.** As a workflow author, I want to sleep for a duration, start and cancel named timers, and wrap a wait with a timeout, using one unit-safe Duration type, so that durable waiting is expressed without unit-ambiguous integers.

**S7.** As a workflow author, I want to wait for a typed signal and register typed query handlers so that external interaction and inspection are as type-safe as the rest of the workflow.

**S8.** As a workflow author, I want to spawn child workflows and fan activities out with all/race/map, with the result types preserved, so that concurrency stays type-safe.

## Test Author — Testing Workflows Without an Engine

**S9.** As a test author, I want to advance simulated time so that a workflow which sleeps for a month is exercised instantly in gleam test, with no wall-clock wait.

**S10.** As a test author, I want to mock an activity with a canned typed result so that I can test workflow logic without dispatching real side effects or running an engine.

**S11.** As a test author, I want to assert that a workflow replays to the same observations so that I catch accidental non-determinism before it reaches production.

## Engine — Resolving the Bindings at Runtime

**S12.** As the engine, I want every SDK primitive to bind to a NIF in one known registry namespace (aion_flow_ffi) so that I can register the native implementations in one place and have compiled workflows resolve against them.

**S13.** As the engine, I want user data to arrive as an encoded payload with a typed codec on the SDK side so that my type-erased event store and replay never need to know the workflow's concrete Gleam types.

## Package Maintainer — Publishing aion_flow to Hex

**S14.** As the package maintainer, I want aion_flow to be pure Gleam with no Rust or engine dependency so that it publishes to Hex standalone and type-checks with no engine present.
