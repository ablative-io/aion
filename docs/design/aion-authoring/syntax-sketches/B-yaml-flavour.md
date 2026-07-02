# B — YAML flavour

> **See sketch G.** Tom's read of this sketch: the document *shape* is the cleanest
> presentation in the folder — the wounds below are format-inheritance problems, not
> shape problems. Sketch G keeps this shape and fixes all three by designing the field
> grammar instead of borrowing GitHub-Actions conventions.

A declarative step list, GitHub-Actions/Argo-style. This is the "obvious" choice for a
workflow config format — which is why its failure modes are so well documented.

```yaml
workflow: research_report

input:
  brief:  Brief        # structured — JSON file, parsed + validated at start
  corpus: Dir          # bulk — content-addressed snapshot handle

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
  - id: questions
    call: plan
    with: [brief]

  - id: findings
    fan_out:
      over: questions
      as: q
      call: investigate
      with: [q, corpus]
    retry:
      attempts: 3
      backoff: 30s

  - id: draft
    call: synthesize
    with: [findings, '""']            # a literal empty string — note the quoting problem

  - id: approval
    wait_for: review
    timeout:
      after: 3d
      then:
        give:
          report: draft
          url: '""'

  # the diamond + rebinding: "draft is replaced iff approval rejected"
  - id: final_draft
    switch:
      - when: "approval.ok"           # an expression… in a string
        value: draft
      - else:
        call: synthesize
        with: [findings, "approval.notes"]   # reference or literal? only convention says

  - id: assets
    call: upload_assets
    with: [final_draft]

  - id: url
    call: publish
    with: [final_draft, assets]
    on_failure:
      - call: delete_assets
        with: [assets]
      - fail: "publish failed"

result:
  report: final_draft
  url: url
```

## What reads well

- Zero uncanny valley — nobody expects YAML to compute, so nobody tries `fetch`.
- Trivially machine-readable; the canvas round-trip is near-free.
- The flat step list superficially resembles the runbook you'd write in English.

## What creaks (and these are the classic, documented wounds)

- **Expressions live in strings.** `"approval.ok"`, `"approval.notes"` — the checker can
  still validate them (we control the parser), but the *reader* gets no highlighting, no
  visual distinction between a reference and a literal, and the `'""'` empty-string
  quoting is genuinely embarrassing. This is the GitHub Actions `${{ }}` wound.
- **The diamond + rebinding needs a contortion.** What was one `if` line in flavour A
  became the seven-line `switch`/`final_draft` block — and every later step must remember
  to reference `final_draft`, not `draft`. The syntax made the *simplest* control flow
  the *most* awkward construct in the file.
- **~105 lines vs ~60** for the identical graph — the ceremony (`- id:` / `call:` /
  `with:`) is per-step overhead that says nothing.
- Indentation as structure at this nesting depth (fan-out body, timeout handler,
  compensation list) is fragile to edit by hand.

## Verdict

**Rejected as the authored surface.** Everything YAML is good at here (machine-readable,
canvas-friendly, diffable) the *canonical JSON serialization* already provides — see
sketch C — without pretending humans should write it. Authoring in YAML buys us the
config-language ceiling: fine until the first branch, painful forever after.
