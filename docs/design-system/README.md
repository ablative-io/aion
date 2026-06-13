# Design System v2

The standard format for how work moves from intent to landed code: roadmap
→ decisions → cluster designs → checklists/stories → briefs → enriched
execution records. JSON is the source of truth for every document; Markdown
is rendered output. Every schema stays inside the `aion codegen` subset, so
the same files that define authoring formats define the workflow's wire
contracts — drift between "what authors write" and "what the pipeline
parses" breaks a test, not a live run.

## Lineage

v1 (`.meridian/design-system/`) standardised the cluster documents and cut
stage prompts from ~70K chars to ~5–9K via progressive enrichment and
reference resolution. v2 keeps all of that and adds what v1 was missing:
the two ledgers above the cluster (roadmap, decisions), first-class stage
contracts, in-place execution records, and authoring/review discipline
informed by the live-dogfood lessons (guessed contracts, attestation vs.
gate, verbatim intent).

## The Document Map

| Document | Scope | Format | Purpose |
|----------|-------|--------|---------|
| `roadmap.json` | project | JSON | Every work item with status, provenance, and links down to clusters/briefs. The dispatcher picks work from here. |
| `decisions.json` | project | JSON | Append-only ADR ledger. Cross-cutting calls (no-arbitrary-defaults, parent-close policy) live once, cited everywhere. |
| `design.json` | cluster | JSON | Architectural anchor — intention, problem, solution, structure, constraints. References ADRs; never restates them. |
| `checklist.json` | cluster | JSON | Verifiable requirements (C#), grouped by section. |
| `stories.json` | cluster | JSON | User stories (S#), grouped by persona. |
| `briefs/{ID}.json` | unit of work | JSON | Requirements (R#) with EARS specs, acceptance, files, C#/S# links — **and** the execution record the pipeline writes back in place. |

Stage contracts (what each pipeline agent must return) are schemas too:

| Schema | Stage | Returned by |
|--------|-------|-------------|
| `scout-report.schema.json` | Scout | read-only codebase exploration |
| `dev-report.schema.json` | Dev | implementation + attestation |
| `review-report.schema.json` | Review | adversarial verdict + hardening |

## File Layout

```
docs/design/
  roadmap.json                  project work ledger
  decisions.json                project ADR ledger
  {cluster}/
    design.json
    checklist.json
    stories.json
    briefs/
      {PREFIX}-001.json         authored spec + accumulated execution record
```

## ID Scheme

- **RM-numbers** — roadmap items (RM-001), sequential per project.
- **ADR-numbers** — decisions (ADR-001), sequential per project, never reused.
- **C-numbers** — checklist items, sequential per cluster.
- **S-numbers** — user stories, sequential per cluster.
- **R-numbers** — requirements, sequential per brief.
- **Brief IDs** — cluster prefix + number (FLOW-001, BUS-003).
- **CN-numbers** — design constraints, sequential per cluster.

Never renumber. Briefs cite C#/S#, designs cite ADR#, requirements cite
both — stable IDs are what make citation cheaper than re-litigation.

## The Lifecycle

A roadmap item moves `idea → designed → briefed → dispatched → landed`
(or `dropped`, with the reason recorded). Each transition has an artifact:
*designed* means a cluster (or ADR) exists and is linked; *briefed* means
coverage is clean (`check-coverage`); *dispatched* means a workflow run
holds it; *landed* means the brief carries its execution record and the
commit is on main. Nothing moves forward on intention alone — the link
field is the proof.

## Enrichment In Place

The brief is one living document. Authors write the spec fields; the
pipeline appends — never rewrites — execution fields:

- per-R#: `scout` (context found), `dev` (what was done, how, deviations,
  per-C#/S# delivery), `review` (verdict per acceptance criterion, issues,
  fixes)
- per-brief: `execution` (workflow id, branch, session, gate results,
  review verdict, landed commit)

The authored fields are the contract; the appended fields are the record.
A reviewer reading the brief six months later sees what was asked, what
the scout found, what the dev claims, what the gate measured, and what
review proved — one file, full provenance. The durable event history in
aion holds the same record append-only if the file is ever doubted.

## Provenance and Verbatim Intent

Load-bearing constraints are recorded as **verbatim quotes** with speaker
and date — in roadmap provenance and in ADR decisions. Paraphrase drifts:
"we shouldn't have a default timeout, the agent steps can take well over
an hour" survived three context compactions because it was quoted, not
summarised. When a requirement traces to something a human said, quote
them.

## Attestation vs. Gate

Pipeline agents attest (`tests_pass: true`); the workflow then runs the
real gate as a recorded activity and stores both. The attestation is never
trusted as the gate — the *divergence* between what the agent believed and
what the gate measured is itself review signal. See `guides/REVIEW.md`.

## Schema Discipline (the codegen subset)

Every schema in `schemas/` must satisfy `aion codegen`: no `$ref`, no
`$defs`, no `oneOf`/`anyOf` unions, no `default`. Shared shapes are
inlined where used (duplication is the cost; a single parseable contract
per document is the payoff). No `default` is policy as much as constraint:
an empty `boundaries` array is the author saying "no boundaries", which is
different from never having been asked. All fields required; emptiness is
explicit.

## Guides

| Guide | What it covers |
|-------|---------------|
| [ROADMAP.md](guides/ROADMAP.md) | The work ledger — item granularity, status discipline, provenance |
| [DECISIONS.md](guides/DECISIONS.md) | ADRs — what is decision-shaped, scope, supersession, quotes |
| [DESIGN.md](guides/DESIGN.md) | Cluster designs — intention, machine-checkable structure, constraints |
| [CHECKLIST.md](guides/CHECKLIST.md) | Checklist items — verifiable, precise, stable |
| [USER-STORIES.md](guides/USER-STORIES.md) | Stories — outcome over implementation, persona discipline |
| [BRIEF.md](guides/BRIEF.md) | Briefs — EARS, acceptance, scope control, the execution record |
| [PROMPTING.md](guides/PROMPTING.md) | Stage prompt projection — budgets, what each stage sees and why |
| [REVIEW.md](guides/REVIEW.md) | Adversarial review — no minor issues, verify the diff, attestation vs. gate |

## Scripts

All in `scripts/`, run with `python3`:

| Script | Usage | What it does |
|--------|-------|-------------|
| `validate.py` | `validate.py <dir-or-file>` | Validates every JSON document against its schema |
| `render-cluster.py` | `render-cluster.py <cluster-dir>` | Renders a cluster to Markdown with resolved references |
| `render-brief.py` | `render-brief.py <brief.json>` | Renders one brief, C#/S#/ADR# resolved, execution record included |
| `render-ledgers.py` | `render-ledgers.py <design-dir>` | Renders roadmap.json + decisions.json to Markdown |
| `check-coverage.py` | `check-coverage.py <cluster-dir>` | Coverage both directions, dependency chain, split-item detection |
| `check-roadmap.py` | `check-roadmap.py <design-dir>` | Status/artifact consistency: every status claim has its link |
