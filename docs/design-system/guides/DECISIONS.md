# The Decision Ledger

`decisions.json` is the project's append-only record of calls that were
made — by a person, on a date, for reasons. Designs cite ADR IDs instead
of restating decisions; briefs carry `design_anchor` ADR lists; the
pipeline projects bound decisions into every stage prompt. One ledger,
cited everywhere, re-litigated nowhere.

## What Is Decision-Shaped

A decision has a **rejected alternative**. "Use thiserror for library
errors" is not an ADR — nothing was at stake. "Required per-spawn
parent-close policy rather than a system default" is an ADR: the
alternative (default to Abandon) was real, considered, and rejected for a
reason that future readers need.

Decision-shaped things that belong here:

- Cross-cutting policies (no arbitrary defaults, no backwards
  compatibility during the build, no default timeouts).
- Architecture forks (event-sourcing over state snapshots; one Recorder
  per workflow).
- Anything a human settled in conversation that work will later depend
  on. **If you find yourself about to write "Tom decided X" in a commit
  message or a memory file, it's an ADR.**

Cluster-local decisions go in the same ledger with `scope` set to the
cluster. Splitting ledgers per cluster recreates the problem the ledger
solves.

## Status Lifecycle

`proposed → decided → superseded`. Proposals are first-class: writing the
recommendation down with its context *as an ADR entry* means the moment
the human says yes, the ledger flips one field instead of someone
reconstructing the reasoning. A superseded entry is never edited beyond
its `superseded_by` field — the successor entry explains what changed and
lists the old ID in `supersedes`.

## Writing the Entry

- **context** — the pressure that forced the fork: the incident, the
  question, the contradiction. A reader who wasn't there must understand
  why doing nothing wasn't an option.
- **decision** — the call AND the rejected road. "X over Y because Z."
  If you cannot name Y, this isn't an ADR.
- **quote** — the decider's verbatim words when they exist. Same
  discipline as roadmap provenance: quotes survive summarisation and
  context compaction; paraphrase drifts. Empty when none.
- **consequences** — the obligations created: engine work implied, doors
  closed, conventions every future brief must observe. These become
  review criteria — a reviewer checks code against the consequences of
  every ADR in the brief's design_anchor.

## Hygiene

- Append-only. Never renumber, never delete, never rewrite a decided
  entry's substance.
- The ledger updates in the same commit as the work that implements a
  decision's consequences, or as the conversation record when decided.
- An ADR cited by no design, brief, or roadmap item after a release
  cycle is a smell: either work is missing or the entry wasn't
  decision-shaped.
