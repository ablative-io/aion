# Writing a Cluster Design

A design is the architectural anchor for a cluster. Every brief, every
implementation decision, and every review question traces back to this
document. It should tell you WHY the system works the way it does, not just
WHAT it does.

## Format

JSON. `design.json` is the source of truth, validated against
`schemas/design.schema.json`; any `DESIGN.md` you see in a cluster directory
is rendered output from `scripts/render-cluster.py`. Edit the JSON,
re-render — never edit the Markdown.

Aim for a rendered document around 400 lines. Long enough to be a genuine
reference; short enough that an agent or human can read the whole thing
before writing a brief.

All fields are required. Emptiness is explicit: an empty `principles` array
is the author saying "the project-level ADRs suffice", which is different
from never having been asked.

## Fields

The fields below are in schema order.

### cluster and title

`cluster` matches the directory under `docs/design/`. `title` is the
human-readable name.

### intention

The spirit of the work — what we're trying to bring about beyond the
immediate technical outcome. What does the world look like when this is
done? How should it feel to use?

This is the philosophical anchor. When a brief author has to make a
judgment call that the documents don't explicitly address, this field is
what they check against. One to three paragraphs. The pipeline projects
intention into stage prompts, so it reaches every agent that touches the
cluster.

### problem

What's broken or missing. Who it affects. Why it matters now rather than
later. Be specific about the pain — vague problems produce vague solutions.

### solution

The design itself: how the pieces fit, module layout, integration points
with other crates or systems, data flow. This is the largest field and the
one most frequently referenced during review.

**What to avoid:**

- Code snippets and type signatures. Those belong in the implementation and
  in briefs. The design describes structure and rationale, not syntax.
- Rationalising standard dependency choices (serde, thiserror, uuid). Only
  discuss dependencies when the choice is non-obvious or constrained.
- Restating decisions. Decisions live in the ledger (see `decisions`
  below) — the solution references ADR IDs where the rationale matters,
  it never re-explains them.
- Repeating information from other fields. If the constraint is stated in
  `constraints`, don't restate it in the solution — cite the CN#.

### principles

Governing principles for this cluster, as `{id, text}` objects with P#
ids (`P1`, `P2`, ...). Keep to 5-10. Principles like "contract is source
of truth" or "no silent fallbacks" resolve review questions before they're
asked, and briefs can cite them by ID.

Empty array when the project-level ADRs already cover the ground — don't
manufacture principles to fill the field.

### decisions

**Decisions no longer live in the design.** They live in the project
decision ledger (`decisions.json`); this field is an array of ADR IDs
(`ADR-007`) the cluster introduced or is bound by. The ledger holds the
content — context, the rejected alternative, the decider's verbatim words,
consequences. See `guides/DECISIONS.md` for what is decision-shaped and
how to write an entry.

The workflow is: while designing, every meaningful trade-off you settle
becomes an ADR entry (or cites an existing one), and its ID lands here.
If there is no rejected alternative, it wasn't a real decision — it was an
obvious step that doesn't need an ADR.

This preserves what made numbered decisions the single most referenced
artefact during brief review: they settle arguments by reference rather
than re-litigation. Every brief can anchor `ADR-007` and the reviewer
verifies against the ledger in seconds — and because the ledger is
project-wide, cross-cutting calls are cited everywhere and restated
nowhere.

### goals

What success looks like, in concrete terms. Each goal should be
verifiable — you can tell whether it was achieved. Keep to 3-7.

Good: "All 19 individual store traits compile and pass existing PG tests
against the new import paths."

Bad: "The system should be well-structured." (Not verifiable.)

### non_goals

Reasonable things we are deliberately not doing in this round, as
`{text, reason}` objects. Each non-goal should be something a reader might
expect to be in scope. `reason` says why it's excluded — empty string only
when the reason is genuinely obvious.

Non-goals protect scope. When a brief author wonders "should I also do
X?", they check here first.

### structure

The file layout the implementation should follow — a **flat,
machine-checkable array** of `{path, note, brief}` objects, not a tree
diagram:

```json
{
  "path": "crates/aion-store/src/error.rs",
  "note": "StoreError and conversion impls",
  "brief": "STORE-002"
}
```

- `path` — repo-root-relative file or directory path.
- `note` — one line: what lives here.
- `brief` — the brief ID that introduces this path; **empty string for
  pre-existing paths**.

This field is essential, not optional. During review, every file path in
every brief is verified against this array — if a path isn't here, it's an
unverifiable claim, and two briefs claiming to create the same path is a
check failure. The `brief` annotation is how you plan the cluster split
and how the checker catches double-create claims; keep it current as
briefs are authored.

### inventory

What exists before this cluster touches it, as `{path, note}` objects —
`note` describes what's there today and its state.

This field prevents the most common existing-code mistake: authors writing
briefs from the design's structure array rather than from the actual
codebase, then declaring files as `create` when they already exist. "File
X is NEW" when it already exists is the second most common review finding;
an inventory kills this category entirely.

Empty array for greenfield clusters. Essential for porting or extraction
work.

### constraints

Things that must not change, as `{id, text}` objects with CN# ids
(`CN1`, `CN2`, ...). Security requirements. Performance expectations.
Compatibility guarantees. Cross-cutting conventions (observability, error
handling, workspace scoping).

Be specific — each constraint should be checkable. "Good performance" is
not a constraint. "Sub-millisecond path normalization for files under 1000
characters" is. CN# ids are stable once assigned; briefs and reviews cite
them, so never renumber.

## What Makes a Design Useful

From 40+ brief reviews across multiple clusters, the patterns are clear:

1. **Decisions with rationale are the most-referenced artefact.** Without
   ADR citations, every review devolves into "was this intentional?" With
   them, the reviewer cites ADR-007 and moves on. The ledger made this
   stronger, not weaker: one entry, cited from every design and brief it
   binds.

2. **The structure array is the second most useful field.** Every R# file
   path is checked against it — now mechanically. Missing or incomplete
   structure means unverifiable briefs.

3. **Intention anchors judgment calls.** Brief authors make dozens of
   small decisions the design doesn't explicitly cover. The intention
   field tells them what the right default is.

4. **Inventory prevents false claims.** An inventory of what already
   exists is how reviewers verify "create" claims without spelunking the
   repo on every brief.

## What Makes a Design Useless

- Prose that describes what the code does without saying why.
- Solutions that read like a feature list without integration context.
- Missing or incomplete structure (forces reviewers to guess at paths and
  breaks the brief checker).
- Decision content restated in the solution instead of cited from the
  ledger — it will drift from the ADR and the two versions will argue.
- Repeating the same information in multiple fields.
- Code snippets that belong in briefs, not designs.
