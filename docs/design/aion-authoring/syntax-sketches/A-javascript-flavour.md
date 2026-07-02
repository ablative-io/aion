# A — JavaScript flavour

> **SUPERSEDED by sketch F (TypeScript subset).** The AI-authorship constraint killed
> this variant: a JS-*lookalike* gives LLM authors plausible-but-wrong priors — every
> drift toward real JS is a syntax error in a language that exists nowhere in training
> data. Sketch F keeps everything that reads well here by making the file real
> TypeScript, so the priors are simply correct and tsc polices the boundary. Kept for
> the record of why.

Braces, `const`/`let`, arrow functions, `if`. Looks like modern JS/TS; **is not JS** — it
is the closed workflow DSL wearing familiar clothes. There is no runtime, no stdlib, no
`fetch`, no `Date`; the only verbs are calling actions, calling pure helpers, and the
orchestration keywords (`fanOut`, `waitFor`, `sleep`, `give`, `fail`, `catch`).

```
workflow research_report {
  input {
    brief:  Brief      // structured — supplied as a JSON file, parsed + validated at start
    corpus: Dir        // bulk — snapshotted into the content store, workflow gets a handle
  }
  output: Published
  signal review: Approval

  types {
    Brief     { topic: String, audience: String, depth: Int }
    Question  { text: String, angle: String }
    Finding   { question: String, summary: String, sources: List(String) }
    Report    { title: String, body: String, findings: List(Finding) }
    Approval  { ok: Bool, notes: String }
    Published { report: Report, url: String }
  }

  run {
    const questions = plan(brief)

    const findings = fanOut(questions, q => investigate(q, corpus), {
      retry: 3,
      backoff: "30s",
    })

    let draft = synthesize(findings, "")

    const approval = waitFor(review, {
      timeout: "3d",
      onTimeout: () => give(Published { report: draft, url: "" }),
    })

    if (!approval.ok) {
      draft = synthesize(findings, approval.notes)
    }

    const assets = uploadAssets(draft)

    const url = publish(draft, assets) catch (e) {
      deleteAssets(assets)
      fail(e)
    }

    give(Published { report: draft, url: url })
  }
}
```

## What reads well

- **Time flows top to bottom**; a step gets a name only when a later line reaches back.
- **The diamond is a non-event**: the revision line just uses `findings` and
  `approval.notes` — both in scope, no ceremony.
- **Rebinding is `let`** — exactly how a JS reader expects a revised draft to work.
- **Conditions are real syntax** (`!approval.ok`), so the checker can typecheck them and
  an editor can highlight them — no expressions-smuggled-into-strings.
- **Compensation as an attached `catch`**: the undo lives visually on the risky step.
- **Durability guarantees hide behind ordinary-looking words**: `waitFor(review …)` parks
  durably for free; `fanOut` is parallel across workers, exactly-once through failover.

## What creaks

- **The uncanny valley** — a JS developer will eventually type `console.log`, `await`,
  `fetch`, `Math.random()`. Every one of those must fail at `aion check` with a message
  that names the fix (`move this into an action…`). This is the whole cost of the flavour;
  it is an error-message engineering problem, not a design problem.
- `catch` here is DSL compensation, not JS try/catch semantics (it fires on exhausted
  retries of THAT step). Possibly rename to `compensate` to break the false association —
  open cosmetic question.
- `Published { … }` record-literal syntax is Gleam-ish, not JS-ish (`new`? plain object
  literal?). Needs one consistent choice.

## Verdict

**Primary candidate.** Largest instantly-fluent audience, handles every stress pattern
without contortion, parses to the graph cleanly for the canvas. Costs one thing —
policing the JS illusion — and that cost is paid in error-message quality, which we would
want to be excellent anyway.
