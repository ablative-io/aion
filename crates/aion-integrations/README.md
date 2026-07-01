# aion-integrations

The harness-integration SDK for [Aion](https://github.com/ablative-io/aion).

Where [`aion-client`](https://docs.rs/aion-client) is the SDK for a caller *driving*
Aion, `aion-integrations` is the SDK for an agent harness Aion *drives*. It defines the
harness-neutral extension seam an integrator implements to make their harness a
first-class Aion integration — plus the reusable building blocks (a generic
JSON-RPC-2.0-over-stdio helper, capability negotiation, and `ActivityEvent` mapping) an
adapter would otherwise hand-roll.

## The seam

Implement [`AgentHarness`] to teach Aion how to run one agent harness. A harness:

- spawns or connects for one activity attempt ([`AgentHarness::start`]),
- advertises which neutral intervention primitives it supports
  ([`AgentSession::capabilities`]) — an **empty set is first-class**: an
  observability-only harness supports no interventions and the ops console offers no
  controls for it,
- streams neutral [`aion_core::ActivityEvent`]s OUT ([`AgentSession::events`]),
- accepts neutral [`aion_core::InterventionCommand`]s IN ([`AgentSession::intervene`]),
- and yields a single terminal [`aion_core::Payload`] result
  ([`AgentSession::wait_result`]).

Nothing in the seam names a concrete harness, a transport, or a wire protocol: the trait
is harness-blind by construction.

## Building blocks

- `jsonrpc` — a generic JSON-RPC 2.0 request/response/notification envelope layer with
  id correlation, newline-delimited framing over any async duplex, and a single
  serializing writer. Any stdio-JSON-RPC harness adapter reuses this instead of
  re-implementing framing.
- Re-exported neutral `aion-core` types, so an integrator has one dependency.

The concrete first-party Norn adapter lives in a separate crate, `aion-integration-norn`;
this SDK never depends on any concrete adapter.
