# Aion-Observability — User Stories

## Operator — Watching a running aion server

**S1.** As an operator tailing the server log, I want a workflow-process crash to print an error-level line with the workflow id, type, and the VM error message the moment it happens so that I never mistake a dead run for a slow one.

**S2.** As an operator, I want a workflow that times out to log at error like a crash does so that a terminal failure is loud regardless of which terminal it reached, given the engine imposes no default timeout of its own.

**S3.** As an operator, I want the failure logged even when the failing process ran on a remote worker the server never hosted so that I can rely on the one log surface I already watch rather than chasing per-worker logs.

## AI Team Member — Dispatching and monitoring a wave of workflows

**S4.** As an AI team member who dispatched a parent run with children, I want a child's failure to show up loudly — on the parent's timeline and in the log — so that I notice a failed child fast instead of waiting on a parent that will never converge.

**S5.** As an AI orchestrator watching the event stream, I want the parent-side child-failure event delivered to my subscription so that I can react to a failed child without polling describe.

## AI Lead — Reviewing a run after a reported failure

**S6.** As an AI lead reviewing why a run died, I want the parent's describe timeline to carry the child's id and its failure reason so that I can see which child failed and why without reconstructing it from separate logs.

**S7.** As an AI lead, I want the failure announcement to be derived from the recorded terminal event rather than a separately stored status so that the log I read and the durable history I audit can never disagree.

## Future Maintainer — Reading the failure-handling code months later

**S8.** As a future maintainer, I want every workflow-process exit to be logged at one funnel so that I can trust that adding a new way to start a workflow cannot reintroduce a silent-exit path.

**S9.** As a future maintainer, I want the logging to be a pure side effect that never alters what is recorded or returned so that I can reason about failure handling and observability independently.
