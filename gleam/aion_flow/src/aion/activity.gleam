//// Typed activity values and policy configuration.

import aion/codec
import aion/duration
import aion/error
import gleam/list
import gleam/option.{type Option, None, Some}

/// Backoff strategy carried with an explicit retry policy.
///
/// The SDK only stores this configuration for the engine to interpret during
/// dispatch. It does not apply retries or invent missing timing values.
pub type Backoff {
  /// Exponential backoff from `initial`, scaled by `multiplier`, capped at `max`.
  Exponential(
    initial: duration.Duration,
    multiplier: Float,
    max: duration.Duration,
  )

  /// Linear backoff from `initial`, adding `increment`, capped at `max`.
  Linear(
    initial: duration.Duration,
    increment: duration.Duration,
    max: duration.Duration,
  )

  /// Fixed backoff with the same `delay` between attempts.
  Fixed(delay: duration.Duration)
}

/// Explicit retry policy for an activity.
///
/// No default retry policy is baked into the SDK. An activity built with `new`
/// and no `retry` decorator carries no policy and runs exactly once when the
/// engine dispatches it.
pub type RetryPolicy {
  RetryPolicy(max_attempts: Int, backoff: Backoff)
}

/// Where an activity's side-effecting body executes.
///
/// The tier is authored data, not an invented default. `aion generate` reads it
/// to decide which worker handler stub and registration entry to emit, and
/// whether a wire-compat golden is generated (remote tiers only). Since the
/// in-VM dispatch tier landed, the tier ALSO rides the runtime `Activity`
/// value as an optional selection (see `execution_tier`): `Some(InVm)` routes
/// the dispatch through the engine's linked in-VM child-process path, while
/// absence (`None`) — and every remote tier — keeps today's remote worker
/// wire. The tier is still never recorded in history: like the dispatcher
/// choice, it is routing, not workflow-visible nondeterminism.
pub type Tier {
  /// Runs in-process inside the BEAM VM via a registered NIF.
  InVm

  /// Runs in a remote Python worker over the worker protocol.
  RemotePython

  /// Runs in a remote Rust worker over the worker protocol.
  RemoteRust
}

/// A typed activity invocation value.
///
/// `i` is the statically-known input type and `o` is the statically-known output
/// type. The input and output codecs are carried so workflow dispatch can encode
/// the author value and decode the type-erased engine payload without
/// reflection.
pub opaque type Activity(i, o) {
  Activity(
    name: String,
    input: i,
    input_codec: codec.Codec(i),
    output_codec: codec.Codec(o),
    runner: fn(i) -> Result(o, error.ActivityError),
    retry_policy: Option(RetryPolicy),
    timeout: Option(duration.Duration),
    heartbeat: Option(duration.Duration),
    labels: List(#(String, String)),
    task_queue: Option(String),
    node: Option(String),
    tier: Option(Tier),
  )
}

/// Build a typed activity value with no retry, timeout, or heartbeat config.
///
/// Absence of config is intentional data: there are no hidden defaults. In
/// particular, an activity with no `retry` decorator carries no policy and an
/// activity with no `execution_tier` decorator dispatches on the remote wire.
pub fn new(
  name: String,
  input: i,
  input_codec: codec.Codec(i),
  output_codec: codec.Codec(o),
  run: fn(i) -> Result(o, error.ActivityError),
) -> Activity(i, o) {
  Activity(
    name: name,
    input: input,
    input_codec: input_codec,
    output_codec: output_codec,
    runner: run,
    retry_policy: None,
    timeout: None,
    heartbeat: None,
    labels: [],
    task_queue: None,
    node: None,
    tier: None,
  )
}

/// Attach an explicit retry policy to an activity.
///
/// Later calls replace earlier retry policy values; the SDK does not merge or
/// synthesize policy fields.
pub fn retry(activity: Activity(i, o), policy: RetryPolicy) -> Activity(i, o) {
  Activity(
    name: activity.name,
    input: activity.input,
    input_codec: activity.input_codec,
    output_codec: activity.output_codec,
    runner: activity.runner,
    retry_policy: Some(policy),
    timeout: activity.timeout,
    heartbeat: activity.heartbeat,
    labels: activity.labels,
    task_queue: activity.task_queue,
    node: activity.node,
    tier: activity.tier,
  )
}

/// Attach an explicit timeout duration to an activity.
pub fn timeout(
  activity: Activity(i, o),
  timeout_duration: duration.Duration,
) -> Activity(i, o) {
  Activity(
    name: activity.name,
    input: activity.input,
    input_codec: activity.input_codec,
    output_codec: activity.output_codec,
    runner: activity.runner,
    retry_policy: activity.retry_policy,
    timeout: Some(timeout_duration),
    heartbeat: activity.heartbeat,
    labels: activity.labels,
    task_queue: activity.task_queue,
    node: activity.node,
    tier: activity.tier,
  )
}

/// Attach an explicit heartbeat interval to an activity.
pub fn heartbeat(
  activity: Activity(i, o),
  heartbeat_interval: duration.Duration,
) -> Activity(i, o) {
  Activity(
    name: activity.name,
    input: activity.input,
    input_codec: activity.input_codec,
    output_codec: activity.output_codec,
    runner: activity.runner,
    retry_policy: activity.retry_policy,
    timeout: activity.timeout,
    heartbeat: Some(heartbeat_interval),
    labels: activity.labels,
    task_queue: activity.task_queue,
    node: activity.node,
    tier: activity.tier,
  )
}

/// Attach a display label to an activity.
///
/// Labels are human-meaningful key/value hints (for example `#("brief",
/// "IP-001")` or `#("repo", "ablative-io/yggdrasil")`) that ride with the
/// dispatch to the worker and surface in its logs and the dashboard. They are
/// display metadata only: the engine never interprets them and they never
/// affect routing, replay, or the recorded history. Repeated calls accumulate
/// in call order; nothing is deduplicated or overwritten.
pub fn label(
  activity: Activity(i, o),
  key: String,
  value: String,
) -> Activity(i, o) {
  Activity(
    name: activity.name,
    input: activity.input,
    input_codec: activity.input_codec,
    output_codec: activity.output_codec,
    runner: activity.runner,
    retry_policy: activity.retry_policy,
    timeout: activity.timeout,
    heartbeat: activity.heartbeat,
    labels: list.append(activity.labels, [#(key, value)]),
    task_queue: activity.task_queue,
    node: activity.node,
    tier: activity.tier,
  )
}

/// Select the task queue this activity is dispatched on (per-activity override).
///
/// The task queue is the routing pool inside the workflow's namespace that a
/// worker subscribes to; selecting it lets one workflow mix activities across
/// pools (for example a `"norn"` step and a `"gpu"` step). This is the
/// highest-precedence selection: it overrides any workflow-level default.
///
/// Absence is intentional data, exactly like retry/timeout/heartbeat: an
/// activity built with `new` and no `task_queue` decorator carries no
/// selection, so the engine resolves it to the workflow-level default when one
/// is set, else the named `"default"` task queue. Later calls replace earlier
/// values; the SDK does not merge.
pub fn task_queue(activity: Activity(i, o), name: String) -> Activity(i, o) {
  Activity(
    name: activity.name,
    input: activity.input,
    input_codec: activity.input_codec,
    output_codec: activity.output_codec,
    runner: activity.runner,
    retry_policy: activity.retry_policy,
    timeout: activity.timeout,
    heartbeat: activity.heartbeat,
    labels: activity.labels,
    task_queue: Some(name),
    node: activity.node,
    tier: activity.tier,
  )
}

/// Pin this activity's dispatch to a specific node (per-activity affinity).
///
/// A node is a concrete worker host inside the activity's `(namespace,
/// task_queue)` pool. Pinning narrows the dispatch from "any worker in the
/// pool" to that one host — for an activity that must run where its state or
/// hardware lives, for example.
///
/// Affinity is OPTIONAL and has NO workflow-level default: an activity built
/// with `new` and no `node` decorator carries no pin (`None`), so the engine
/// dispatches it to any worker in the pool. There is no precedence to resolve —
/// the activity either pins a node or it does not. Later calls replace earlier
/// values; the SDK does not merge.
pub fn node(activity: Activity(i, o), name: String) -> Activity(i, o) {
  Activity(
    name: activity.name,
    input: activity.input,
    input_codec: activity.input_codec,
    output_codec: activity.output_codec,
    runner: activity.runner,
    retry_policy: activity.retry_policy,
    timeout: activity.timeout,
    heartbeat: activity.heartbeat,
    labels: activity.labels,
    task_queue: activity.task_queue,
    node: Some(name),
    tier: activity.tier,
  )
}

/// Select the execution tier this activity dispatches on.
///
/// `InVm` routes the dispatch through the engine's in-VM path: the activity's
/// runner executes ONCE, live, inside a linked child process of the workflow
/// process — no remote worker, no task-queue routing — while its recorded
/// history (`ActivityScheduled`/`ActivityStarted`/terminal) and replay
/// semantics stay byte-identical to a remote dispatch. Remote tiers
/// (`RemotePython`/`RemoteRust`) keep the remote worker wire; selecting one is
/// equivalent to selecting nothing today.
///
/// Absence is intentional data, exactly like retry/timeout/heartbeat: an
/// activity built with `new` and no `execution_tier` decorator carries no
/// selection (`None`) and dispatches on the remote wire — today's exact
/// behavior. Later calls replace earlier values; the SDK does not merge.
pub fn execution_tier(activity: Activity(i, o), tier: Tier) -> Activity(i, o) {
  Activity(
    name: activity.name,
    input: activity.input,
    input_codec: activity.input_codec,
    output_codec: activity.output_codec,
    runner: activity.runner,
    retry_policy: activity.retry_policy,
    timeout: activity.timeout,
    heartbeat: activity.heartbeat,
    labels: activity.labels,
    task_queue: activity.task_queue,
    node: activity.node,
    tier: Some(tier),
  )
}

/// Return the activity name used by the engine dispatch boundary.
pub fn name(activity: Activity(i, o)) -> String {
  activity.name
}

/// Return the display labels attached to the activity, in call order.
pub fn labels(activity: Activity(i, o)) -> List(#(String, String)) {
  activity.labels
}

/// Return the typed input captured by the activity value.
pub fn input(activity: Activity(i, o)) -> i {
  activity.input
}

/// Return the typed input codec captured by the activity value.
pub fn input_codec(activity: Activity(i, o)) -> codec.Codec(i) {
  activity.input_codec
}

/// Return the typed output codec captured by the activity value.
pub fn output_codec(activity: Activity(i, o)) -> codec.Codec(o) {
  activity.output_codec
}

/// Return the typed runner captured by the activity value.
pub fn runner(
  activity: Activity(i, o),
) -> fn(i) -> Result(o, error.ActivityError) {
  activity.runner
}

/// Return the explicitly attached retry policy, if one exists.
pub fn retry_policy(activity: Activity(i, o)) -> Option(RetryPolicy) {
  activity.retry_policy
}

/// Return the explicitly attached timeout duration, if one exists.
pub fn timeout_duration(activity: Activity(i, o)) -> Option(duration.Duration) {
  activity.timeout
}

/// Return the explicitly attached heartbeat interval, if one exists.
pub fn heartbeat_interval(
  activity: Activity(i, o),
) -> Option(duration.Duration) {
  activity.heartbeat
}

/// Return the explicitly selected per-activity task queue, if one exists.
///
/// Absence (`None`) means no override was set, so the engine resolves the
/// dispatch to the workflow-level default, else the named `"default"` queue.
pub fn selected_task_queue(activity: Activity(i, o)) -> Option(String) {
  activity.task_queue
}

/// Return the explicitly pinned per-activity node affinity, if one exists.
///
/// Absence (`None`) means no pin was set, so the engine dispatches the activity
/// to any worker in its `(namespace, task_queue)` pool. Unlike the task queue,
/// node affinity has no workflow-level default: `None` is final.
pub fn selected_node(activity: Activity(i, o)) -> Option(String) {
  activity.node
}

/// Return the explicitly selected execution tier, if one exists.
///
/// Absence (`None`) means no `execution_tier` decorator was applied, so the
/// dispatch takes the remote worker wire — today's exact behavior.
pub fn selected_tier(activity: Activity(i, o)) -> Option(Tier) {
  activity.tier
}

/// A typed binding of a value type's name to its codec.
///
/// `declare` takes one for the input and one for the output. The codec makes
/// the binding type-check — `type_ref("OrderInput", order_input_codec())` only
/// compiles when the codec is a `codec.Codec(OrderInput)` — so a mismatched
/// type and codec is a `gleam build` failure. The `type_name` is the handle the
/// out-of-process `aion generate` extractor maps to `schemas/<type>.json` to
/// drive codec and worker codegen; codecs carry no type information at runtime,
/// so the name is supplied explicitly here.
pub opaque type TypeRef(a) {
  TypeRef(type_name: String, codec: codec.Codec(a))
}

/// Bind a value type's name to its codec for use in an activity declaration.
pub fn type_ref(type_name: String, value_codec: codec.Codec(a)) -> TypeRef(a) {
  TypeRef(type_name: type_name, codec: value_codec)
}

/// Return the codec captured by a type reference.
pub fn type_ref_codec(reference: TypeRef(a)) -> codec.Codec(a) {
  reference.codec
}

/// A type-erased activity declaration: the single per-activity artifact an
/// author writes.
///
/// `declare` captures the typed input and output `TypeRef`s — so the
/// declaration's contract is checked by `gleam build` — and erases them to the
/// names the generator needs. Erasure at the boundary is what lets a package's
/// activities, which have different input and output types, live together in
/// one `List(Declaration)` (mirroring Aion's type-erased event history). From
/// this declaration `aion generate` derives the `activity.new` wrapper, the
/// value-type codec pairs, the worker handler stub, the registration entry, the
/// `workflow.toml` entry, and (for remote tiers) the wire-compat golden. No
/// retry/timeout/heartbeat lives here: absence is intentional (ADR-001), so it
/// is structurally impossible for codegen to emit a policy the author did not
/// choose.
pub opaque type Declaration {
  Declaration(name: String, tier: Tier, input_type: String, output_type: String)
}

/// Declare an activity from its name, tier, and typed input/output references.
///
/// This is the only per-activity artifact an author hand-writes; the plumbing
/// is generated from it. The side-effecting body (the runner) is written
/// separately and referenced by the generated wrapper — codegen never
/// synthesizes a body (the determinism boundary is unchanged).
pub fn declare(
  name: String,
  tier: Tier,
  input: TypeRef(i),
  output: TypeRef(o),
) -> Declaration {
  Declaration(
    name: name,
    tier: tier,
    input_type: input.type_name,
    output_type: output.type_name,
  )
}

/// Render a tier as the canonical string the generator's wire form uses.
pub fn tier_to_string(tier: Tier) -> String {
  case tier {
    InVm -> "in_vm"
    RemotePython -> "remote_python"
    RemoteRust -> "remote_rust"
  }
}

/// Return the engine-facing name of a declared activity.
pub fn declaration_name(declaration: Declaration) -> String {
  declaration.name
}

/// Return the tier a declared activity runs on.
pub fn declaration_tier(declaration: Declaration) -> Tier {
  declaration.tier
}

/// Return the input value type name of a declared activity.
pub fn declaration_input_type(declaration: Declaration) -> String {
  declaration.input_type
}

/// Return the output value type name of a declared activity.
pub fn declaration_output_type(declaration: Declaration) -> String {
  declaration.output_type
}
