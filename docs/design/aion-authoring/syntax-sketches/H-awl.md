# H — AWL (aion workflow language, owned carrier)

Added after Tom's ruling (2026-07-04): **our own tiny workflow language** (#217 AWL-0).
YAML-likes and markdown are dead for authoring; markdown is generated docs only. The
format must smell like the product.

H keeps everything that made G read well — the runbook shape, load-bearing `about`,
time flowing down the page, rebinding, exactly one world-touching verb — and drops the
YAML carrier entirely. No quoting rules, no `no` → boolean, no indentation ambiguity
inherited from a config format we don't control. The grammar is ours end to end, so
`aion check` and the printer are the *definition*, not a validator bolted onto someone
else's parser.

Two carrier decisions that make it smell like the product:

1. **Prose needs no quotes.** `about` takes the rest of the line as text, because the
   console narrates runs in the author's words and authors shouldn't escape their own
   narration. Strings inside *expressions* are quoted as normal.
2. **Steps are the unit of everything** — authoring, checking, canvas nodes, console
   narration, versioning. A step body is fields, one per line. If a unit of work needs
   more than one call, it's a child workflow — same rule G had for fan-out, applied
   uniformly (fan-out bodies *and* cycle bodies).

```awl
workflow research_report
about Research a brief, gate it through a durable human review, publish.

input brief: Brief      // structured — parsed + validated from brief.json at start
input corpus: Dir       // bulk — content-addressed snapshot handle (haematite)
output Published

signal review: Approval

type Brief     { topic: String, audience: String, depth: Int }
type Question  { text: String, angle: String }
type Finding   { question: String, summary: String, sources: List(String) }
type Report    { title: String, body: String, findings: List(Finding) }
type Approval  { ok: Bool, notes: String }
type Published { report: Report, url: String }

action plan(brief: Brief) -> List(Question)
action investigate(q: Question, corpus: Dir) -> Finding
action synthesize(findings: List(Finding), notes: String) -> Report
action upload_assets(report: Report) -> List(String)
action publish(report: Report, assets: List(String)) -> String
action delete_assets(assets: List(String)) -> Nil

step plan
  about Break the brief into distinct research questions.
  do plan(brief)
  as questions

step investigate
  about One agent per question, parallel across workers, exactly-once on failover.
  each q in questions
  do investigate(q, corpus)
  retry 3 every 30s
  as findings

step first_draft
  do synthesize(findings, "")
  as draft

step human_review
  about Durable gate — parks free while idle, survives restarts, resumable weeks later.
  wait review
  timeout 3d
  on timeout
    finish Published(report: draft, url: "")
  as approval

step revise
  about One revision pass if the reviewer pushed back.
  when not approval.ok
  do synthesize(findings, approval.notes)
  as draft                       // rebinds — every later step sees the revised draft

step upload_assets
  do upload_assets(draft)
  as assets

step publish
  do publish(draft, assets)
  on failure
    do delete_assets(assets)
    fail
  as url

finish Published(report: draft, url: url)
```

And the construct G had no answer for — dev_brief's bounded fix cycles — stays flat
with the same one-call rule (the round is a child workflow, independently observable):

```awl
step fix_rounds
  about Bounded fix cycles: developer, gates, adversarial review — until accepted.
  repeat up to config.max_fix_cycles
  do child fix_round(brief: brief, config: config, state: state)
  as state
  until state.accepted
```

## The grammar in one breath

Declarations: `workflow` / `about` / `input` / `output` / `error` / `signal` / `type` /
`action` / `step` / `finish`. Step fields: `about`, `when` (guard), `each … in …`
(fan-out), `do` (action call, or `do child` for a child workflow), `wait` (signal gate),
`sleep`, `repeat up to … / until` (bounded cycle), `retry n every d` (or
`retry n backoff d..d`), `timeout`, `on timeout` / `on failure` handler blocks,
`as` (binding — rebinding is the same word), routing overrides `queue` / `node`.
Expressions are G's micro-grammar unchanged: literals, references, field access, calls,
record construction, `not/and/or`, comparisons. Durations are literals: `30s`, `10m`,
`2h`, `3d`. Comments `//`. That is the whole language.

## What reads well

- Everything G scored on, minus its honest costs: still a runbook, still narrates
  itself, still nothing to learn to *read* — but no YAML sharp edges to disclaim, and
  the expression fields aren't second-class scalars smuggled through a config parser;
  the whole file is one grammar with one checker and compiler-quality errors everywhere.
- **AI authorship**: the grammar is small enough to specify exhaustively in a system
  prompt, and it resembles nothing — no plausible-but-wrong priors from lookalike
  syntaxes (the trap the 2026-07-02 addendum named for TypeScript-subset).
- **Lossless parse ↔ print** is trivial by construction: fields have a canonical order,
  one per line; the printer is the formatter (`awl format`, no check mode — there is
  exactly one rendering).

## Deliberate constraints (features, not gaps)

- One world-touching verb (`do`), engine-mediated time (`sleep`, `timeout`) — the
  language has no vocabulary for clock, randomness, or I/O. Determinism by construction.
- One-call bodies for `each` and `repeat` — multi-step units are child workflows. Keeps
  documents flat, the canvas clean, every composite independently versioned.
- No general recursion, no unbounded loops, no user-defined functions. Real computation
  lives in actions behind the contract boundary.

## Honest costs

- We own a parser, typechecker, and printer (AWL-0's actual build) plus eventual editor
  support. Mitigation: the language is deliberately tiny, and `crates/aion-package/src/
  structure/` already defines the graph model the parser targets.
- A new syntax has no ecosystem. But the ecosystem YAML bought us was exactly the part
  that hurt.

## Verdict sought

Cold-read instrument for Tom, per the fixture-based measurement in
VESPER-LYND-METHOD-NOTES §3: does this smell like the product? Ratifying H unblocks the
AWL-0 build (parser → typechecker → printer → fixture goldens), with the interpreter
tier (#216) behind it and a beamr bytecode emitter as the north star.
