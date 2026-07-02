# G — Step document (YAML shape, designed field grammar)

Added after Tom's steer (2026-07-02): the declarative document presentation "comes across
so cleanly" — define the types/schemas, then lay out the control flow as a simple
sequence of described steps, "borderline almost like markdown: here's step one, here's
what happens during it." TypeScript-as-surface didn't sit right; the pull is toward a
*document*, not a program.

This sketch keeps flavour B's document shape and fixes its three wounds by **designing
the format instead of inheriting config-file conventions**:

1. **Expressions get one tiny, familiar grammar** (calls, field access, literals,
   `not/and/or`, comparisons) in designated fields (`do:`, `when:`, `finish:`, args).
   YAML sees a plain scalar; `aion check` parses and typechecks every expression against
   the action contracts — the error quality of a real compiler, in a data file.
2. **Conditional rebinding kills the `switch` contortion**: a step with `when:` + `as:`
   replaces the named value when the condition holds; otherwise the old value flows on.
3. **References vs literals are unambiguous** because calls look like calls:
   `synthesize(findings, "")` — bare names are references, quotes are strings.

The file is **real YAML** — standard parsers, editor support, and a published JSON
Schema, so AI authors get zero syntax surprises and (with structured output) can be
*prevented* from emitting an invalid document at all.

```yaml
workflow: research_report

input:
  brief: Brief          # structured — parsed + validated from brief.json at start
  corpus: Dir           # bulk — content-addressed snapshot handle (haematite)

output: Published

signals:
  review: Approval

types:
  Brief:     { topic: String, audience: String, depth: Int }
  Question:  { text: String, angle: String }
  Finding:   { question: String, summary: String, sources: List(String) }
  Report:    { title: String, body: String, findings: List(Finding) }
  Approval:  { ok: Bool, notes: String }
  Published: { report: Report, url: String }

steps:
  - plan:
      about: Break the brief into distinct research questions.
      do: plan(brief)
      as: questions

  - investigate:
      about: One agent per question, parallel across workers, exactly-once on failover.
      each: q in questions
      do: investigate(q, corpus)
      retry: { attempts: 3, backoff: 30s }
      as: findings

  - first draft:
      do: synthesize(findings, "")
      as: draft

  - human review:
      about: Durable gate — parks free while idle, survives restarts, resumable weeks later.
      wait: review
      timeout: 3d
      on timeout:
        finish: Published(report: draft, url: "")
      as: approval

  - revise:
      about: One revision pass if the reviewer pushed back.
      when: not approval.ok
      do: synthesize(findings, approval.notes)
      as: draft                # rebinds — every later step sees the revised draft

  - upload assets:
      do: upload_assets(draft)
      as: assets

  - publish:
      do: publish(draft, assets)
      as: url
      on failure:
        do: delete_assets(assets)
        then: fail

finish: Published(report: draft, url: url)
```

Compare the `revise` step (4 meaningful lines) with flavour B's seven-line
`switch`/`final_draft` contortion for the same graph node — the rebinding rule is what
makes the document shape viable.

## What reads well

- **It reads as a runbook.** Step names are headings; `about:` is the author explaining
  each one. Time flows down the list. Nothing here needs programming-language fluency to
  *read* — the original adoption goal, met more directly than any other flavour.
- **`about:` is load-bearing, not decoration.** It flows to the canvas node label, to a
  rendered-markdown doc view (`aion doc` — the workflow *is* its own documentation), and
  to the ops console during a run — **a running workflow narrates itself in its author's
  words**, step by step. (Direct fit with the NOI observability direction.)
- **AI authorship is the safest of any flavour.** Structure is schema-validated YAML
  (constrained decoding can make invalid documents unemittable); the only free-form code
  surface is the expression micro-grammar, small enough to specify exhaustively in a
  system prompt.
- Determinism by construction, unchanged: the document has no vocabulary for clock,
  randomness, or I/O. The only world-touching verb is calling an action.

## Deliberate constraints (features, not gaps)

- **A fan-out body is one call.** `each:` + `do:` — if an item needs multiple steps,
  that's a child workflow (`do: run investigate_deeply(q)`), which keeps documents flat,
  the canvas clean, and every multi-step unit independently observable/versionable.
- **No expressions beyond the micro-grammar.** Real computation lives in helpers/actions
  behind the contract boundary, same rule as every flavour.

## Honest costs

- **Editor intelligence inside expression fields** (highlighting `not approval.ok` as
  code, autocompleting action names) needs a small LSP/editor extension eventually;
  until then, `aion check` is the feedback loop. The *structure* gets schema-driven
  autocomplete in every YAML-aware editor from day one.
- **YAML's own sharp edges** (quoting rules, `no` → boolean, indentation) — mitigated by
  the schema + `aion check`, and by keeping values that could be misparsed (durations,
  expressions) in fields whose types the checker knows. If these ever hurt in practice,
  the same document shape ports to a stricter carrier format without changing the model.
- Verbosity sits between the flavours: ~75 lines here vs ~60 (F) vs ~105 (B) — the
  `about:` lines account for most of the difference and are doing real work.

## Relationship to the other flavours

The canonical model makes this a *primary surface*, not the *only* one. Sketch F
(TypeScript subset) remains fully compatible as a later code-first **view** for people
and AI harnesses that prefer it — "multiple languages we could work from" is exactly
what model-in-the-middle buys; we just don't build extra views now. Canonical JSON (C)
stays the serialization + structured-output lane. Nobody ever edits the generated Gleam
— it has the same status as compiler output in `target/`.

## Verdict

**Primary candidate, superseding F, pending Tom's read of this file.** It matches the
customer's stated aesthetic (three independent pulls toward the document shape), meets
the AI-authorship constraint *better* than a code surface (schema-validated data beats
subset-linted code for drift), keeps the adoption story ("readable without a tutorial")
strongest, and gives the ops console its narration for free.
