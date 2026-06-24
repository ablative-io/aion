# Aion — Variable-Liveness Workflow Testing + Specialised Workflow-Authoring Agent (idea)

> **Status: idea capture, 2026-06-22 (Tom, dictated).** Raw idea, written down before it was lost.
> Not yet scoped or scheduled. Two linked concepts: (1) a workflow test harness with a *dial* for how
> "live" the run is, and (2) a specialised Norn agent that uses that harness to author workflows in a loop
> until they're verified-working. Relates to the existing `aion dev` command, the brief-dispatch loop, Norn,
> and the distributed-Aion direction.

## Part 1 — Variable-liveness workflow testing (a "liveness dial")

Extend beyond today's `aion dev` command: a way to test a workflow at **varying degrees of liveness**, so you
can validate progressively without always paying full cost. The spectrum:

- **Mocked (types only).** Supply **mock inputs and outputs** and confirm everything lines up — all the
  **types line up**, the wiring is sound. Essentially a compile-time / structural check of the workflow:
  does each step's output type feed the next step's input, do the activities' signatures match, etc.
- **Real-but-cheap.** Run with **real inputs and real outputs**, but with **test/cheap models** (smaller,
  cheaper providers) to **minimise costs** while still exercising real execution.
- **Sandboxed / full live.** Run it for real — **in a VM or sandbox** if isolation matters — full liveness.

The point is a single dial: mock → cheap-real → full-live, so you can catch type/wiring errors for free,
then validate behaviour cheaply, then confirm end-to-end — without jumping straight to an expensive live run.

## Part 2 — Specialised Norn workflow-authoring agent

A **Norn integration specialised for authoring Aion workflows** — its whole job is to write a workflow for
you, correctly, so you (or another caller) don't have to.

How it works:
- You **describe the workflow you want** — what you need, the things it has to do.
- The agent is **highly specialised toward that use case** — it carries all the specific instructions for
  writing good Aion workflows (whether that's via stock-standard skill documentation or otherwise).
- It **asks the user a few questions** and **works through it** interactively: "okay, cool — what do we get
  for this? what do we get for that? how do you want to use X?" — clarifying the shape and intent.
- Then it **works through a loop**, writing the workflow for you, and **iterates until it gets it right.**

The loop uses **Part 1's liveness dial** as its verification ladder. By the end, the goal is to hand back a
workflow that:
- **passes compile-time safety checks**,
- has **run through the dry runs** (the mocked / cheap-real liveness levels), and
- **works** — verified to the point where it actually runs.

So the **caller never has to go through that themselves.** The caller can be a human ("it'd be handy to have
a workflow for this") **or another AI** — either sets off the request to the specialised agent, which **loops
around getting it right** and returns a verified, working workflow.

## Why the two halves belong together
Part 1 is the *verification substrate*; Part 2 is the *author that drives it in a loop*. The authoring agent
can only promise "verified-working" because the liveness dial lets it cheaply prove types line up, then prove
behaviour on cheap models, then confirm live — iterating against each rung until the workflow passes. Together
they turn "I wish I had a workflow for X" (from a person or an agent) into a delivered, dry-run-passed workflow.

## Threads it connects to (for later scoping)
- Builds on the existing `aion dev` command (the starting point Tom referenced).
- The "author in a loop until verified" pattern mirrors the brief-dispatch loop, but specialised for Aion
  workflow generation rather than code briefs.
- Natural fit with the distributed-Aion / remote-worker direction (cheap-real and sandboxed runs could go to
  remote/VM workers).
- Another-AI-as-caller makes this a composable capability (an agent requesting a workflow from Aion).
