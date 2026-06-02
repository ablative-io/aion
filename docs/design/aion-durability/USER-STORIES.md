# Aion-Durability — User Stories

## Engine — Recording and Replaying Workflows

**S1.** As the engine, I want a single writer per workflow that owns the sequence head so that every event lands with the correct expected sequence and the log never races or develops gaps.

**S2.** As the engine, I want to replay a workflow from its recorded history and have every activity, timer, signal, and child call return its recorded outcome so that I reconstruct the exact prior state without repeating side effects.

**S3.** As the engine, I want replay to tell me the resume point so that I know precisely where live execution should continue.

**S4.** As the engine, I want activity results served from recorded history during replay so that a completed activity is never dispatched a second time.

**S5.** As the engine, I want a single seam (LiveExecutor) that is called only after the resume point so that replay performs no live side effect and live execution is cleanly separated.

## Operator — Recovering and Diagnosing

**S6.** As an operator, I want every active workflow to be replayed and resumed automatically on engine startup so that no in-flight workflow is silently lost after a restart.

**S7.** As an operator, I want a non-deterministic workflow to fail loudly with an error that names the workflow, the position, and what diverged so that I can find and fix the offending workflow code instead of debugging corrupted state.

**S8.** As an operator, I want a failure recovering one workflow not to abort recovery of the others so that one bad workflow cannot take down the whole engine restart.

## Workflow Author — Writing Deterministic Workflows

**S9.** As a workflow author, I want workflow.now to return the recorded event time and workflow.random to return a workflow-seeded value so that my workflow replays to exactly the same state every time without me thinking about it.

**S10.** As a workflow author, I want replay to be invisible so that I write a normal deterministic function and never code differently for the replay case.
