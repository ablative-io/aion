# Aion-Time-Signals — User Stories

## Workflow Author — Writing Durable, Interactive, Concurrent Workflows

**S1.** As a workflow author, I want my workflow to sleep for an arbitrary duration and resume after a VM restart so that long delays are durable, not lost on a crash.

**S2.** As a workflow author, I want to start a named timer and cancel it later so that I can model deadlines and SLAs that may be retired before they elapse.

**S3.** As a workflow author, I want to wait for a named signal and have it delivered the instant it arrives so that my workflow reacts to external events without polling.

**S4.** As a workflow author, I want a signal sent while my workflow was momentarily not resident to still reach me so that no external event is silently dropped.

**S5.** As a workflow author, I want to spawn a child workflow and await its result so that I can compose larger processes from smaller ones, each with its own durable history.

**S6.** As a workflow author, I want to fan work out with all, race the fastest of several with race, and fan out dynamically from data with map so that I can express concurrency directly without writing a join runtime.

## Engine — Driving Live Interaction and Recording It

**S7.** As the engine, I want every timer firing, signal arrival, and child outcome recorded as an aion-core Event so that replay can return the same observation without re-waiting or re-spawning.

**S8.** As the engine, I want a timer fired exactly once whether it fires on the live wheel or via recovery so that replay sees one TimerFired, never two and never none.

**S9.** As the engine, I want to recover elapsed timers on startup and on a periodic tick so that a workflow that slept across a restart wakes correctly.

**S10.** As the engine, I want cancellation to terminate losing or remaining child processes and record it so that no orphaned children leak and replay reconstructs which children were cancelled.

## Operator — Inspecting Live Workflows

**S11.** As an operator, I want to query a running workflow's current state without recording anything or disturbing its execution so that I can observe progress safely on a live system.

**S12.** As an operator, I want a query against an unresponsive, terminal, or unknown workflow to return a clear typed error rather than hang so that my tooling stays responsive.

## Replay Engine (AD) — Consuming the Recorded Events

**S13.** As the replay engine, I want recorded events to carry exactly what I need (timer id + fire_at, signal name + payload, child id + result/error, spawn correlation) so that on restore I can return recorded observations deterministically without re-running live delivery.
