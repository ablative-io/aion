# D — Nix flavour

An expression language of attribute sets and `let … in`. Philosophically the closest to
what a workflow *is* (a pure dataflow graph with effects only at named boundaries), which
makes it the most instructive sketch — and still the wrong choice.

```nix
{
  workflow = "research_report";

  input = {
    brief  = Brief;      # structured — JSON file, parsed + validated at start
    corpus = Dir;        # bulk — content-addressed snapshot handle
  };
  output  = Published;
  signals = { review = Approval; };

  types = {
    Brief     = { topic = String; audience = String; depth = Int; };
    Question  = { text = String; angle = String; };
    Finding   = { question = String; summary = String; sources = List String; };
    Report    = { title = String; body = String; findings = List Finding; };
    Approval  = { ok = Bool; notes = String; };
    Published = { report = Report; url = String; };
  };

  run = { brief, corpus, review, ... }:
    let
      questions = plan brief;

      findings = fanOut questions (q: investigate q corpus) {
        retry = 3;
        backoff = "30s";
      };

      draft0 = synthesize findings "";

      approval = waitFor review {
        timeout   = "3d";
        onTimeout = give { report = draft0; url = ""; };
      };

      draft = if approval.ok then draft0
              else synthesize findings approval.notes;

      assets = uploadAssets draft;

      url = publish draft assets {
        onFailure = [ (deleteAssets assets) fail ];
      };
    in
      give { report = draft; url = url; };
}
```

## What reads well

- **The `if` expression handles the diamond beautifully** — `draft = if approval.ok then
  draft0 else synthesize …` is arguably the cleanest revision step in the whole folder.
- **Sequencing is data flow.** Steps order themselves by their dependencies, which is the
  semantic truth (that IS the graph). Nothing runs "after" anything except because it
  uses its result.
- Attribute-set config blocks (`{ retry = 3; … }`) are tidy and uniform.

## What creaks

- **`draft0`.** Pure bindings can't rebind, so the natural "replace the draft" becomes
  shadow-numbering. One revision loop is fine; three become `draft0/draft1/draft2` — the
  wart grows with exactly the workflows that need revising most.
- **Implicit sequencing cuts both ways**: people *think* in time order, and here the
  execution order must be inferred by reading the dependency edges. The canvas doesn't
  care, but a human scanning the file does.
- **Juxtaposition application** (`plan brief`, `investigate q corpus`) reads as a typo to
  every mainstream developer. And Nix syntax fluency is a niche within a niche — adopting
  it *maximizes* the unfamiliarity we set out to remove (the original complaint about
  Gleam, amplified).
- `onFailure = [ … ]` — effect ordering inside a pure expression language is where the
  elegance visibly runs out; compensation is imperative by nature.

## Verdict

**Rejected.** Keep two of its ideas — the `if`-expression *form* for conditional binding
is worth stealing into flavour A's checker rules, and "sequencing = data flow" is exactly
what the canonical graph records — but the surface itself optimizes for a reader we don't
have.
