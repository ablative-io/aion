# Policy: Worker Reconnect Drop Budget and Clean-Close Semantics (task #46)

Decision record from the 2026-06 remediation, approved by Tom. Implementation wave (all three worker SDKs) dispatched the same day.

## Problem

Two cross-SDK policy gaps surfaced by the reconnect-hardening reviews (#48, #43 follow-ups):

1. **The drop budget never resets.** After the TS unbounded-reconnect fix, all three SDKs count dropped connection attempts against a cumulative budget that is exhausted once and never replenished. A worker that serves healthily for a month, then weathers a brief network blip, still spends from the same lifetime budget — eventually a long-lived worker dies on a transient drop that a freshly started worker would have survived. The budget was meant to stop reconnect *storms*, not to impose a lifetime cap on recoveries.
2. **Clean server close is treated inconsistently.** A server that closes the stream cleanly (normal close frame / `Ok(None)` end-of-stream) was treated as terminal in some SDKs and retryable in others. A clean close during a rolling server restart should not kill the worker fleet.

## Decision (Pair A)

1. **Budget reset rule:** the cumulative drop budget resets to full when a session is *demonstrably healthy*, defined as either:
   - the session **served at least one task** (dispatch delivered and result reported), OR
   - the session **outlived the maximum backoff delay** (it stayed connected longer than the schedule's longest wait, so it cannot be part of a tight crash loop).

   No new configuration: both signals derive from existing state (task accounting and the configured backoff schedule). A storm still exhausts the budget — short-lived sessions that never serve and never outlast max backoff never reset it.

2. **Clean server close = retryable, budgeted drop in ALL three SDKs.** A clean close consumes budget like any other drop (so a server that close-loops a worker still terminates it), but it is never instantly terminal. Non-retryable errors (auth, registration denial) keep their precedence and still fail fast.

## The proper fix (deferred to the #39/#47 proto wave, approved)

The clean-close ambiguity exists because the wire protocol has no way for the server to say *why* it is closing. The second proto wave adds an explicit **drain signal**: a server-initiated frame meaning "reconnect elsewhere / after restart," distinct from "you are not welcome." Once that lands, the clean-close heuristic above is replaced by the explicit signal:

- drain frame → reconnect without consuming budget,
- denial-class status → terminal,
- unannounced close → budgeted retryable drop (network ambiguity remains).

This rides with the #39 register-ack and #47 result-ack/deadline additions so the worker protocol breaks once, not three times (NO BACKWARDS COMPATIBILITY — the wire contract is replaced, not versioned alongside).

## Affected code

- Rust: `crates/aion-worker/src/worker.rs` (clean-close classification at the run-loop), reconnect budget state in `protocol/reconnect.rs`.
- Python: `sdks/python/aion-worker/aion_worker/loop.py` (clean-close classification), reconnect bookkeeping.
- TypeScript: `sdks/typescript/aion-worker/src/{loop,reconnect}.ts` (already retryable on clean close; gains the reset rule).

Each SDK's wave includes regression tests for: reset-after-served-task, reset-after-outliving-max-backoff, storm-still-exhausts, clean-close-consumes-budget, and denial-still-beats-everything ordering.
