# The Roadmap Ledger

`roadmap.json` is the project's single answer to "what work exists, where
did it come from, and where is it now?" It is machine-readable on purpose:
the dispatcher workflow selects briefed work from it, and `check-roadmap.py`
verifies that every status claim is backed by its artifact.

## What Goes In

One item per coherent deliverable a person asked for or a review surfaced.
The granularity test: **could this item move through the lifecycle on its
own?** "CLI JSON ergonomics" is one item even though it becomes several
briefs; "fix the banner count and also redesign children" is two items
wearing one id.

Everything enters as `idea` — including things you're confident about.
The ledger is not a promise list; `idea` costs nothing and preserves
provenance before it evaporates.

## Status Is Earned by Artifacts

| Status | The artifact that earns it |
|--------|---------------------------|
| `idea` | the entry itself, with provenance |
| `designed` | `links.cluster` names a cluster whose design.json exists, and/or `links.decisions` names the ADRs |
| `briefed` | `links.briefs` lists briefs and `check-coverage.py` is clean for their cluster |
| `dispatched` | a workflow run holds the briefs (the brief's `execution.workflow_id` is set) |
| `landed` | `links.commits` lists the commits on main; the briefs carry landed execution records |
| `dropped` | `notes` carries the reason — a dropped item with no reason is a dangling question forever |

Never advance status on intention. The check script flags `briefed` items
with empty `links.briefs` because that is a claim without its receipt.

## Provenance: Quote the Human

The `provenance.quote` field is for **verbatim words when they carry
load-bearing intent**. "we shouldn't have a default timeout, the agent
steps can take well over an hour" is a quote that settled a design argument
months later; a paraphrase ("timeouts should be long") would have lost the
constraint entirely. Rules:

- Quote exactly, typos and all. The quote is evidence, not prose.
- Empty string when no human words carry the requirement (e.g. an item
  born from a CI failure). Don't manufacture quotes.
- `context` says where the words come from — a conversation, an incident,
  a review — enough that a reader can judge the weight.

## Dependencies

`depends_on` lists direct prerequisites only, by RM-number. The dispatcher
refuses to dispatch an item whose dependencies haven't landed. Transitive
chains are computed, not written.

## Hygiene

- IDs are never reused, items are never deleted — `dropped` is the
  terminal for abandoned work.
- `updated` changes on every edit; it's the staleness signal.
- The ledger is updated in the same commit as the work that changes an
  item's status. A roadmap that lags the repo is worse than none — it
  reports false state with confidence.
