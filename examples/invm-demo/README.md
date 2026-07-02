# invm-demo — the in-VM activity tier, end to end

The smallest possible proof of Aion's in-VM execution tier: one workflow, one
activity, **no worker**. The `shout` activity is decorated with

```gleam
|> activity.execution_tier(activity.InVm)
```

so its runner (a pure-Gleam transform) executes once, live, inside a linked
child process of the workflow process — no task queue subscription, no remote
worker deployment, nothing to run besides the server itself. Everything
recorded is byte-identical to a remote dispatch (`ActivityScheduled` /
`ActivityStarted` / `ActivityCompleted` with task queue, node, and attempt
stamped), so:

- **replay** returns the recording without re-executing the runner;
- **kill -9 mid-activity** (after `Started`, before a terminal) recovers via
  the replay-reopen path — the recovered engine re-executes workflow code,
  which re-supplies the runner thunk, and the activity re-dispatches with a
  fresh reopen `ActivityScheduled` for the same ordinal;
- a **runner crash** kills only the child process and records a proper
  terminal `ActivityFailed` — the workflow observes it as a typed error.

## Run it

```sh
# Build and package the workflow.
aion package examples/invm-demo

# Deploy to a running server and start it — no worker step.
aion deploy examples/invm-demo/invm-demo.aion
aion start invm_demo --input '{"name": "sydney"}'
```

The result is `"SYDNEY!!!"`. For the failover proof: start a run, kill the
server between `ActivityStarted` and the completion (the window is small for
this instant transform — add a sleep in `local_shout` to widen it while
experimenting), restart, and watch the run complete with the correct result.

The end-to-end regression for this example (including the crash/replay proof
at the engine level) lives in `crates/aion/tests/examples_e2e.rs` and
`crates/aion/tests/invm_activity_e2e.rs`.
