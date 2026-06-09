# Aion-Nif-Bridge — User Stories

## Workflow Author — Writing durable workflows in Gleam using the aion_flow SDK

**S1.** As a workflow author, I want my activity calls to be durable so that if the engine crashes and restarts, the activity result is returned from history instead of re-dispatching to the worker.

**S2.** As a workflow author, I want workflow.now to return a deterministic timestamp so that my workflow logic produces the same decisions on replay as on first execution.

**S3.** As a workflow author, I want workflow.random to return a deterministic value so that randomised branching in my workflow replays correctly.

**S4.** As a workflow author, I want to sleep for a duration and have the engine wake my workflow after that time so that I can implement polling, backoff, and scheduled delays.

**S5.** As a workflow author, I want to receive signals from external callers so that my workflow can react to events like approvals, cancellations, or data updates.

**S6.** As a workflow author, I want to register query handlers so that external callers can inspect my workflow's current state without modifying it.

**S7.** As a workflow author, I want to spawn child workflows and await their results so that I can compose complex workflows from simpler building blocks.

**S8.** As a workflow author, I want to dispatch multiple activities in parallel and collect all results so that I can fan out work for better throughput.

**S9.** As a workflow author, I want to race multiple activities and take the first result so that I can implement timeout patterns and competitive dispatch.

## Platform Operator — Running and monitoring Aion workflows in production

**S10.** As a platform operator, I want completed workflows to show a full event history so that I can audit what happened, when, and in what order.

**S11.** As a platform operator, I want workflow completion to be detected automatically so that workflows do not stay in Running status after their process exits.

**S12.** As a platform operator, I want crashed workflows to be recoverable by restarting the engine so that I have Temporal-class durability guarantees.

## Temporal Developer — Evaluating Aion as a Temporal replacement

**S13.** As a Temporal developer, I want the getting-started tutorial to demonstrate a complete workflow lifecycle (start, activity, complete, describe history) so that I can evaluate whether Aion meets my durability expectations.

**S14.** As a Temporal developer, I want activities, timers, signals, queries, and child workflows to work as I expect from Temporal so that I can port my existing workflow patterns without surprises.
