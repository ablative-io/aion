# Stage Prompt Projection

Every pipeline stage receives a **deterministic projection** of the
enriched brief — a pure function from documents to prompt, implemented in
workflow code, unit-tested like any codec. No stage ever receives a whole
corpus; no stage ever receives less than it needs to act without guessing.
This guide defines what each stage sees and the discipline behind it.

## The Budget Principle

v1 proved the numbers: scout ~5K chars, dev ~7K, review ~9K — down from
~70K when documents were forwarded wholesale. The mechanism is filtering
plus **reference resolution**: a requirement citing C4 receives C4's text
inline (`C4 — StorageError defined in libmessage::storage::error`), not
the 86-item checklist it came from. Budgets are review criteria for the
projection functions: a stage prompt that grows past its budget means the
projection has started forwarding instead of filtering.

Agents have file tools. The projection's job is orientation, not
completeness — give the agent what it needs to *start right* (the brief,
resolved references, the binding decisions, where to look), and a path to
everything else (the design file reference). Forwarding what the agent
can read on disk is paying twice.

## What Every Stage Gets

- **Instructions** — static per stage, ~500 chars, imperative.
- **Decision context** — the ADRs in the brief's `design_anchor`,
  projected as `id: title — decision` lines. Decisions bind every stage;
  this is how no-arbitrary-defaults reaches an agent that has never seen
  the ledger.
- **Verbatim intent** — when the roadmap item or an ADR carries a quote,
  the quote rides the prompt **as quoted words with a speaker**, never
  paraphrased into the instructions. The agent should know what the human
  actually said.
- **Design context** — intention + constraints + goals from the cluster
  design (~800 chars). Never `problem`/`solution`/`structure` prose —
  scout reads the codebase, the requirements carry the paths.
- **Boundaries** — the brief's SHALL NOTs, always, every stage. Scope
  protection only works if no stage is exempt.
- **The design file reference** — a path, not the content.

## Per Stage

### Scout (read-only)

Requirements with resolved C#/S# text. No prior enrichment exists.
Instructions emphasise: 2-5 files per R# with line ranges, conventions,
a concrete approach, gotchas — saving the dev time, not cataloguing.

### Dev

Requirements **with scout blocks rendered inline** per R#. The dev does
not re-explore what the scout established; deviation from the scouted
approach is allowed but must be declared (the `deviation` field — silent
deviation is a review finding). The dev is told the workflow runs the
real gate afterwards: implement and attest, don't burn the session
running the full suite.

### Review

Requirements with scout AND dev blocks inline, the dev's attestation,
the gate's measured results, and the brief's verification steps. The
review prompt's first instruction is the discipline that matters most:
**verify the actual diff, not the report** (see REVIEW.md).

## Enrichment Is Append-Only

Each stage returns its report against the stage schema
(`scout-report` / `dev-report` / `review-report`); the workflow validates,
then appends each enrichment to its R# **in place** in the brief. Authored
fields are never touched by the pipeline. A stage that needs to amend its
own earlier output (the gate-failure fix loop) resumes its session and
returns a full replacement report — the workflow replaces that stage's
blocks wholesale, never merges field-by-field.

## Sessions

Stage sessions use **deterministic ids derived from brief id + stage**
(mint-or-resume): a crashed run that recovers resumes the same agent
session with its context intact, instead of a fresh agent rediscovering
everything. This is what makes the durable-workflow + agent combination
actually durable end to end.

## The Contract Rule

Every prompt builder and every parser in the pipeline is written against
a schema in `schemas/`, and every test fixture for them is a **captured
real output, never an invented one**. The live-dogfood night's lesson:
five consecutive run failures were imagined contracts (guessed argv,
guessed response envelopes), and not one had a test that could fail.
Contract drift must break a test, not a live run.
