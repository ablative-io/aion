# E — Pipe flavour (Gleam/Elixir-shaped)

The `|>` pipeline aesthetic. Included because it's the house style of the BEAM world we
live in, and because it demos gorgeously — right up until the workflow stops being a
straight line.

The linear spine, where pipes shine:

```
workflow research_report {
  input  { brief: Brief, corpus: Dir }
  output Published
  signal review: Approval

  run =
    brief
    |> plan
    |> fan_out(fn(q) { investigate(q, corpus) }, retry: 3, backoff: "30s")
    |> synthesize("")
    // ...and here the music stops
}
```

Four steps, four lines, zero names. Nothing else in this folder is this dense or this
pretty. But now the durable gate and the revision:

```
  run = {
    let findings =
      brief
      |> plan
      |> fan_out(fn(q) { investigate(q, corpus) }, retry: 3, backoff: "30s")

    let draft = synthesize(findings, "")

    let approval = wait_for(review, timeout: "3d",
                            on_timeout: give(Published(report: draft, url: "")))

    // the diamond: needs findings AND approval — the pipe carries only one value
    let draft = case approval.ok {
      True  -> draft
      False -> synthesize(findings, approval.notes)
    }

    let assets = upload_assets(draft)

    let url = publish(draft, assets)
      |> on_failure(fn(e) {
           delete_assets(assets)
           fail(e)
         })

    give(Published(report: draft, url: url))
  }
```

## What reads well

- The **linear sub-chains** (`brief |> plan |> fan_out(…)`) are the best-reading
  fragments in the entire folder — no intermediate names for values nobody reaches back to.
- Elixir/Gleam natives feel instantly at home; it's the house aesthetic of the substrate.

## What creaks

- **The diamond kills the pipeline.** The moment a step needs two upstream values
  (`findings` + `approval.notes`), you're back to `let`-bindings — which is to say, back
  to flavour A with different punctuation. The honest version of this file is "flavour A
  wearing a Gleam shirt, plus four nice lines at the top."
- The temptation it creates is worse than the syntax: authors will **bundle steps into
  helper stages to keep the pipe pretty** (`|> investigate_all |> review_loop`), which
  hides orchestration steps from the graph — the canvas shows one opaque node where five
  steps live. A syntax that rewards hiding the thing we exist to show is working against
  the product.
- `case`/`fn` blocks re-import the Gleam-fluency requirement we're trying to remove for
  the ordering layer.

## Verdict

**Rejected as the organizing principle; shortlisted as sugar.** If flavour A wins, a
later, purely-cosmetic addition — allowing `a |> b |> c` as sugar for nested calls *on
linear runs only*, desugaring to the same graph — captures everything good here at zero
structural cost. Not in v1.
