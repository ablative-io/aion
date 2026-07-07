# retry-policy

Minimal workflow exercising the **engine-honored per-activity retry policy**
(#197).

The workflow schedules one remote `flaky_call` activity that declares:

```gleam
|> activity.retry(activity.RetryPolicy(
  max_attempts: 3,
  backoff: activity.Fixed(delay: duration.milliseconds(25)),
))
```

The workflow body contains **no retry logic**. When a worker fails an attempt
with a retryable error (`ActivityFailure.retryable(..)` in the worker SDK —
the `retryable:` reason prefix on the wire), the engine's dispatch seam:

1. records the failed attempt durably as a non-terminal `ActivityFailed`
   (kind `Retryable`),
2. waits out the declared backoff,
3. re-dispatches the SAME activity (same ordinal, same routing) at the
   incremented attempt,

up to `max_attempts` total attempts. A worker flake costs a retry, not the
run. Exhausted budgets fail the workflow with the last reason verbatim and
the final attempt count recorded on the terminal event; the failed run stays
reopenable (`aion reopen`), and a reopened re-dispatch continues the attempt
trail instead of restarting it.

An activity with no `retry` decorator keeps the SDK contract: it runs exactly
once.

Exercised end-to-end by `crates/aion/tests/activity_retry_e2e.rs`, which
rebuilds this package from source on every run.
