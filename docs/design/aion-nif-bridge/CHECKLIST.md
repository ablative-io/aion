# Aion-Nif-Bridge — Checklist

## NIF Context and Infrastructure

- [ ] **C1** — NifContext resolves a calling BEAM PID to its WorkflowHandle via the Registry.
- [ ] **C2** — NifContext provides synchronous access to the workflow's Recorder from a dirty NIF thread via tokio Handle::block_on.
- [ ] **C3** — WorkflowHandle wraps its Recorder in Arc<Mutex<Recorder>> for interior-mutable access from the NIF bridge.
- [ ] **C4** — engine_nifs.rs registers NIF entries for all 18 aion_flow_ffi functions.
- [ ] **C5** — Every NIF checks recorded history via the Resolver before executing a side effect.

## Activity NIF

- [ ] **C6** — run_activity/3 NIF records ActivityScheduled and ActivityStarted events before dispatching.
- [ ] **C7** — run_activity/3 NIF records ActivityCompleted on success or ActivityFailed on terminal failure.
- [ ] **C8** — run_activity/3 on replay returns the recorded result without dispatching to a worker.
- [ ] **C9** — The workflow_id in activity events matches the actual workflow's WorkflowId, not a random UUID.

## Deterministic NIFs

- [ ] **C10** — now/0 returns the recorded_at timestamp of the last event in the workflow's history.
- [ ] **C11** — random/0 returns a deterministic float seeded from WorkflowId + RunId + sequence position.
- [ ] **C12** — random_int/2 returns a deterministic integer in the given range seeded from WorkflowId + RunId + sequence position.
- [ ] **C13** — now, random, and random_int produce identical values on replay as on first execution.

## Timer NIFs

- [ ] **C14** — sleep/1 NIF records TimerScheduled, blocks until the timer fires or is cancelled, and records TimerFired.
- [ ] **C15** — start_timer/2 NIF records TimerScheduled and returns the timer ID.
- [ ] **C16** — cancel_timer/1 NIF records TimerCancelled for the named timer.
- [ ] **C17** — with_timeout/2 NIF wraps an operation with a deadline; records timeout expiry if the deadline fires first.
- [ ] **C18** — Timer NIFs on replay return recorded results without scheduling real timers.

## Signal NIFs

- [ ] **C19** — receive_signal/2 NIF blocks until a signal arrives and records SignalReceived.
- [ ] **C20** — send_signal/3 NIF delivers a signal to a target workflow and records SignalSent.
- [ ] **C21** — Signal NIFs on replay return recorded results without delivering or waiting for real signals.

## Query NIFs

- [ ] **C22** — register_query/3 NIF registers a query handler for the workflow.
- [ ] **C23** — reply_query/2 NIF sends a query response.
- [ ] **C24** — dispatch_query/2 NIF dispatches a query to a workflow and returns the response.

## Child Workflow NIFs

- [ ] **C25** — spawn_child/3 NIF starts a child workflow and records ChildWorkflowStarted.
- [ ] **C26** — await_child/1 NIF blocks until the child completes and records ChildWorkflowCompleted or ChildWorkflowFailed.
- [ ] **C27** — Child NIFs on replay return recorded results without spawning real child processes.

## Concurrency NIFs

- [ ] **C28** — collect_all/2 NIF dispatches multiple activities and waits for all results.
- [ ] **C29** — collect_race/2 NIF dispatches multiple activities and returns the first result, cancelling the rest.
- [ ] **C30** — collect_map/2 NIF dispatches activities from a mapped collection.
- [ ] **C31** — Concurrency NIFs record individual activity events for each dispatched activity.

## Process Exit Detection

- [ ] **C32** — The engine monitors each workflow process via beamr process monitoring.
- [ ] **C33** — Normal process exit records WorkflowCompleted with the return value as the result payload.
- [ ] **C34** — Abnormal process exit records WorkflowFailed with the exit reason as the error payload.
- [ ] **C35** — After terminal event recording, the workflow handle's cached status is updated in the Registry.
- [ ] **C36** — Zombie workflows (Running status with dead process) do not occur after this cluster lands.
