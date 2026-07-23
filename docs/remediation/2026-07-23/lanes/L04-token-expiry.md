# L04 — Worker token expiry on every frame + dispatch selection

**Findings:** F-2 (major, security) · **Risk:** small · **Depends on:** — · **Status:** ready

Closes the revocation gap: an idle worker with an expired JWT currently keeps its authenticated, dispatch-eligible stream indefinitely.

## dev_brief input

```json
{
  "brief": {
    "id": "rem-l04-token-expiry",
    "title": "Enforce worker token expiry on every stream frame and on dispatch selection (F-2)",
    "objective": "crates/aion-server/src/api/worker_grpc.rs enforces token_expired() ONLY inside the Message::Heartbeat arm of the inbound loop (single call site, around line 405). Heartbeats are per-in-flight-activity — the heartbeat tracker rejects heartbeats with no matching task — so an idle worker emits none and is never checked: a worker whose short-lived JWT expired keeps its authenticated stream and registry entry indefinitely and continues receiving new activity dispatches. Credential rotation/revocation is defeated by simply not heartbeating. Fix in three parts: (1) hoist the expiry check out of the Heartbeat arm so EVERY inbound frame (Result, Heartbeat, and any other message) and every loop iteration is subject to it — including a stream that goes fully quiet (the check must fire on a time basis, not only on frame receipt; use the loop's existing wakeup/select structure rather than adding a new timer mechanism if one fits). (2) Enforce expiry at dispatch selection: an expired worker must not be selected for new activity dispatch, independent of its stream state. (3) On expiry past the existing grace window, deregister the worker (through the existing teardown path, which the review verified is idempotent with a drop-guard sweep) and terminate the stream with the existing unauthenticated status. Preserve the current grace-window semantics (expired_since + heartbeat_grace) — the change is WHERE the check runs, not the grace policy. Preserve the existing send-error-then-terminate shape so workers get the re-authentication signal before the stream closes.",
    "context": "Source review: docs/REVIEW-23-07.md F-2, independently re-confirmed at current main bytes by Vesper (single token_expired call site in the Heartbeat arm; token_expired helper at ~line 471). The review verified the rest of the server's authn/authz defense-in-depth clean — this is the one gap; do not go looking for redesigns beyond it. In-flight activities on an expiring worker: decide explicitly whether they run to completion within the grace window or are re-queued on deregistration, matching whatever the existing worker-death path does (the reconciler already handles worker loss); document the choice in the dev report.",
    "pointers": [
      "docs/REVIEW-23-07.md (F-2)",
      "crates/aion-server/src/api/worker_grpc.rs (inbound loop, token_expired, expired_since/heartbeat_grace)",
      "crates/aion-server/src/worker/heartbeat.rs (per-task heartbeat rejection — why idle workers emit no heartbeats)",
      "the worker registry / dispatch selection path (find where dispatch-eligible workers are selected)",
      "the existing worker teardown/deregistration path (idempotent, drop-guard sweep)"
    ],
    "scope_in": [
      "crates/aion-server/src/api/worker_grpc.rs",
      "The dispatch-selection site, minimally (expiry predicate at selection)",
      "The deregistration call site, minimally",
      "Tests: server-side worker lifecycle tests"
    ],
    "scope_out": [
      "NO changes to the token format, JWT validation, or claims model",
      "NO changes to the grace-window policy or its durations",
      "NO changes to the worker-side SDKs or the wire protocol",
      "NO changes to the heartbeat tracker's per-task semantics",
      "Workspace laws: no unwrap/expect/panic (tests included), no #[allow], typed errors, files ≤500 lines"
    ],
    "acceptance": [
      "A connected worker that sends NO frames after its token expires is denied within the grace window and deregistered (evidence: test with a short-expiry token and a silent worker)",
      "An expired worker is excluded from dispatch selection even before stream teardown completes (evidence: test asserting no new dispatch lands on an expired worker)",
      "A worker with a valid token and active heartbeats is unaffected (existing tests stay green)",
      "The expiry path routes through the existing idempotent teardown (no new teardown mechanism)",
      "Grace-window semantics unchanged for the heartbeat case (existing behavior pinned by test)"
    ],
    "notes": "The time-basis check on a quiet stream is the part most likely to be done wrong (e.g. only checking on frame arrival reproduces the bug for fully-silent workers). The test for the silent-worker case must not depend on the worker sending anything at all."
  },
  "config": {
    "repo_root": "{REPO_ROOT}",
    "base_branch": "main",
    "gates": [
      {"name": "fmt", "argv": ["cargo", "fmt", "--all"]},
      {"name": "clippy", "argv": ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]},
      {"name": "test", "argv": ["cargo", "test", "--workspace"]}
    ],
    "verify_gates": [
      {"name": "clippy", "argv": ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]},
      {"name": "test", "argv": ["cargo", "test", "--workspace"]}
    ]
  }
}
```
