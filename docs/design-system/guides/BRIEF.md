# Writing an Implementation Brief

A brief is a single unit of work that a workflow can execute — and, once
dispatched, its execution record. One living document: authors write the
spec; the pipeline appends what the scout found, what the dev did, what the
gate measured, and what review proved. It maps checklist items and user
stories to concrete requirements with file paths, EARS-notation specs, and
acceptance criteria. The implementing agent should be able to complete the
brief without going back to the design for missing details.

## Format

JSON, validated against `schemas/brief.schema.json`. Rendered to Markdown
by `scripts/render-brief.py` with C#/S#/ADR# references resolved and the
execution record included.

Every authored field is required. **Emptiness is authored, not
defaulted** — an empty `boundaries` array is the author saying "no
boundaries", which is different from never having been asked. The
enrichment fields (`scout`/`dev`/`review` per requirement, `execution` per
brief) are the only optional ones, because the pipeline appends them later.

## Authored Fields

### Identification

- `id` — Cluster prefix + three-digit number (FLOW-001, BUS-003).
- `cluster` — Cluster name matching the directory under `docs/design/`.
- `title` — Short imperative description of what the brief delivers.

### Dependencies

- `depends_on` — Brief IDs that must land before this one can start. Only
  list direct dependencies, not transitive ones — the dispatcher orders
  by these.
- `blocked_by` — External blockers that aren't briefs (e.g. a pending ADR,
  an upstream API change).

### Cross-References

- `checklist` — Array of C-numbers this brief covers.
- `stories` — Array of S-numbers this brief addresses.

These are the single source of truth for what's assigned to this brief.
The checklist and stories documents have no mapping tables — coverage is
tracked here, and `scripts/check-coverage.py` verifies it.

**Write cross-references first, then requirements.** List every checklist
item and story this brief must deliver, then write R#s that deliver them.
Not the reverse. Backfilling cross-references after writing R#s is the most
common source of coverage mismatches.

**Check coverage in both directions.** Forward: every C-number in the brief
header must appear in at least one R#'s checklist. Backward: every C-number
in an R# must appear in the brief header. Mismatches mean the brief claims
coverage it doesn't deliver, or an R# is doing unclaimed work.

**When a checklist item is split across briefs** (e.g. stub in brief A,
wiring in brief B), both briefs must note the split in their task
descriptions. Otherwise the second brief claims work the first already
did. `check-coverage.py` flags multi-brief items so the notes can be
checked.

**Don't mix namespaces.** C-numbers are checklist items. S-numbers are user
stories. ADR-numbers are decisions. They are different namespaces.

### design_anchor

Array of ADR IDs that bind this brief. The pipeline projects these
decisions into **every stage prompt** — scout, dev, and review all see the
anchored decisions as `id: title — decision` lines, and the reviewer checks
the change against the consequences of every anchored ADR. This is how a
cross-cutting call like no-arbitrary-defaults reaches an agent that has
never seen the ledger.

Anchor the decisions that genuinely constrain this work — the ones a dev
could plausibly violate. Don't anchor the whole ledger: each anchored ADR
costs prompt budget and creates a review obligation. See
`guides/DECISIONS.md` for the ledger and `guides/PROMPTING.md` for how
anchors ride the prompts.

### purpose

One paragraph — what this brief delivers and why it matters. Reference the
design for architectural context; don't repeat it here.

### task

Plain-language description of what needs to be done. This is what the
implementing agent reads first to orient themselves. Include the scope
boundary: what's in, what's out. Split-item notes and
deviation-from-checklist notes live here too.

### requirements

Array of R# objects. Each requirement is a discrete piece of work.

**Size:** 5-10 R#s is the sweet spot. Under 5 is suspiciously thin — check
that you haven't lumped too much into a single R#. Over 12 is a split
signal — break the brief into two.

**Primary file rule:** Every R# should have at least one file in its files
section that no other R# in the same brief touches as its primary target.
If two R#s share the same primary file, they're probably one requirement
split artificially. Exception: a module-wiring R# that touches mod.rs
across several modules.

**Separate "build" from "wire" briefs.** If a brief is trying to both build
a component and wire it into the system, consider splitting. Build briefs
are focused and review cleanly. Wiring briefs have broader scope but
shallower per-file depth.

Each R# has:

#### id and title

`R1`, `R2`, etc., sequential per brief. Title is a short imperative
statement.

#### spec (EARS notation)

Requirements use EARS (Easy Approach to Requirements Syntax):

- **Event-driven**: WHEN {trigger}, THE SYSTEM SHALL {response}
- **State-driven**: WHILE {precondition}, THE SYSTEM SHALL {response}
- **Unwanted**: IF {condition}, THEN THE SYSTEM SHALL {response}
- **Complex**: WHILE {precondition}, WHEN {trigger}, THE SYSTEM SHALL {response}

SHALL = absolute requirement. SHOULD = strong recommendation.
MAY = optional. SHALL NOT = prohibition.

**Include SHALL NOT clauses.** If your EARS spec has no SHALL NOT, ask
yourself what the requirement prohibits. The interesting constraints live
in the negative space. "SHALL validate the envelope signature AND SHALL NOT
accept self-signed envelopes" is more useful than "SHALL validate the
envelope signature" alone.

**EARS is for behavioural requirements.** For structural requirements (move
X to Y, define type Z with fields A/B/C), use plain language. Don't write
artificial EARS to satisfy the format — it produces awkward specs that
obscure the actual requirement.

#### acceptance

Observable conditions that prove the requirement is done. This is where
review lives or dies — the reviewer returns a verdict **per criterion**,
with evidence from the actual diff, so a criterion that can't be checked
produces a verdict that means nothing.

**The assert test:** Every criterion must be writable as an assert
statement. If you can't write `assert_eq!(result, expected)` from it, it's
too vague.

Good: "`ContractFilter::default()` imposes no constraints."
Good: "`peer_public_key` is `[u8; 32]` not `Vec<u8>`."
Good: "Parses a multi-diagnostic stderr block into separate `DiagnosticEvent` values."

Bad: "The adapter works correctly." (Not observable.)
Bad: "All six methods compile and pass tests." (What tests? What behaviour?)
Bad: "Paths are normalized to repo-root-relative." (Tautology — restates the spec.)

**Concrete inputs, concrete outputs.** When a criterion involves a
transformation, name the specific case: "/home/user/project/src/lib.rs
with repo root /home/user/project/ produces src/lib.rs."

**No ambiguous "or".** Acceptance criteria must be definitive. If two
approaches are valid, pick one. The implementing agent needs one path, not
a choose-your-own-adventure.

**Technical claims must be accurate.** If a criterion says "LEFT JOIN
gracefully handles missing columns via COALESCE", that claim must be true.
Don't handwave about how a technology behaves — verify it.

#### files

Paths to create, modify, or delete. **All three keys — `create`, `modify`,
`delete` — are required, with explicit empty arrays.** An empty `delete`
is the author saying "this requirement deletes nothing"; the schema has no
defaults, so silence isn't an option.

**Trace the call chain.** If data flows through layer X, layer X's file
belongs in the file list. Listing an API endpoint without the service
wrapper it calls through is an incomplete file list.

**Verify files against the codebase.** Before declaring a file as `create`,
check that it doesn't already exist. Authors writing from the design's
structure array rather than from the actual code is the most common source
of false "create" claims — and every path is also verified against that
structure array, so a path missing from the design fails the check.

**Derive types from actual code.** When modifying existing types, check the
real struct fields. PG schemas derived from mental models instead of actual
Rust types produce column-type mismatches.

#### checklist and stories (per-R#)

Which C-numbers and S-numbers this specific requirement delivers. Must be a
subset of the brief-level arrays.

**Deviations from checklist descriptions:** When the R# delivers a
checklist item differently than the checklist text describes (different
signature, different scope, deferred sub-feature), add a note in the task
field or in the requirement's spec. Otherwise the reviewer has to ask
whether the deviation is intentional.

### boundaries

SHALL NOT statements — scope protection. What this brief must not do. This
is how you prevent implementing agents from over-reaching into work that
belongs to future briefs. Boundaries are projected into every stage
prompt, no stage exempt — write them as if each one will be read by an
agent mid-implementation, because it will.

### verification

Steps a reviewer should follow to verify the whole brief was implemented
correctly. These complement per-R# acceptance criteria with cross-cutting
checks (cargo check, grep for stale imports, etc.). The reviewer runs each
step and records pass/fail, so write steps that can actually be executed.

## The Execution Record (appended by the pipeline)

The remaining fields are **appended by the pipeline, never authored, and
never touched by authors** — not even to pre-fill placeholders. The
pipeline appends them in place and never rewrites the authored fields
above. The authored fields are the contract; the appended fields are the
record. A reader six months later sees what was asked, what the scout
found, what the dev claims, what the gate measured, and what review
proved — one file, full provenance.

Per requirement:

- `scout` — read-only exploration findings: 2-5 key files with line
  ranges, conventions and gotchas to match, a one-paragraph approach,
  and edge cases the brief may not have considered.
- `dev` — what was done: status (implemented/blocked), files actually
  changed, how the requirement was met (rationale, not diff narration),
  any deviation from the scouted plan (empty means the plan was followed —
  silent deviation is a review finding), and per-C#/S# delivery claims.
- `review` — the adversarial verdict: alignment (aligned/drifted/fixed),
  a verdict **per acceptance criterion** with evidence from the actual
  diff (file:line or test name, not the dev's claim), C#/S# verified
  delivered, issues found (no severity field — there are no minor
  issues), and what the harden pass fixed.

Per brief:

- `execution` — the run-level record: workflow id, branch, deterministic
  session id, the **gate** results (what the workflow measured: fmt,
  clippy, tests) alongside the dev's **attestation** (what the agent
  believed), the review verdict, and the landed commit. Divergence between
  attestation and gate is itself review signal.

Why authors should care about fields they never write: every acceptance
criterion you author becomes a per-criterion review verdict, every
boundary becomes a stage-prompt constraint, every anchored ADR becomes a
review obligation. Sloppy authored fields produce an execution record that
can't prove anything. The deep discipline lives in `guides/PROMPTING.md`
(how stages see the brief) and `guides/REVIEW.md` (how the record is
verified).

## Structural Discipline

**mod.rs contains ONLY `pub mod` declarations and `pub use` re-exports.**
No functions, traits, structs, or enums. If you're putting logic in mod.rs,
it goes in a named file. This is the single most common structural error
across all clusters.

**If a concept appears in multiple R#s, extract it.** Inconsistent path
handling, duplicated error types, or repeated config loading across R#s
in the same brief should be a shared R# that others depend on.

**If an R# responds to events with a specific code, something must produce
those events.** Either another R# in this brief or a prerequisite brief.
Dead policies and dead auto-fix declarations are the result of skipping
this check.

**LOC awareness.** If your estimate for any single file exceeds 300 lines,
add a test extraction strategy. Authors estimate implementation LOC but
forget that tests add 50-100%.

## Scope-Forward Additions

If you add something beyond what the checklist calls for (an extra method,
an additional store, a utility type), call it out explicitly with
justification. Don't silently expand scope. The reviewer will catch it
and ask anyway.

## Mutation vs Observation

When a brief introduces mutation that records who did what (respond to a
message, approve a request, sign a document), recipient/owner validation
is not deferrable. Any authenticated user being able to act on someone
else's behalf is a security gap, not a follow-up item.
