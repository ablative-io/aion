# AWL worked examples

Three example AWL documents, authored against
[`AWL-0-SPEC-DRAFT.md`](../AWL-0-SPEC-DRAFT.md) (the language contract) and the
canonical fixtures under `crates/aion-awl/tests/fixtures/`. They exist to show
what a complete, coherent AWL workflow looks like — not fragments — and to
exercise the language across its rev-0 (implemented) and AWL-1 (spec'd,
ratified, not yet implemented) surface.

Each file carries its rev level in a leading comment. **rev-0** files pass
`aion awl check` today. **AWL-1** files are aspirational: they use constructs
the spec ratifies but `aion-awl` does not yet parse or typecheck, so
`aion awl check` rejects them — the exact diagnostic is quoted below so the
gap is legible rather than surprising.

Concrete syntax follows the committed fixtures exactly: action arguments are
**positional** (`do provision_workspace(config.repo_root, config.base_branch, brief)`),
`as` is its own line and comes last, durations are literals (`30s`, `5m`,
`2d`), and `about` prose runs unquoted to end of line.

Verified with the installed toolchain: `aion 0.8.0` (`aion awl check`).

---

## A. `dev_brief.awl` — rev-0 (implemented; checks clean)

The `examples/dev-brief` pipeline re-expressed in AWL: provision an isolated
worktree, drive a bounded developer/gate fix cycle until the mechanical gates
are green (or the cycle budget is spent), fan out one adversarial review lens
per configured charter, adjudicate the verdicts, rework once more if the review
rejected the round, then clean up with a compensating teardown. It shows the
full rev-0 data-plumbing vocabulary: a rich `type` model for the brief and
results, bindings threading step to step, field access inside a `when` guard
and inside a `repeat`/`until` bound, `each` fan-out over the configured lenses,
and an `on failure` compensation block.

One faithfulness compromise is forced by rev-0, not chosen: the real pipeline
runs each fix round and each review lens as **child workflows** and adjudicates
their results, but rev-0 child results are opaque (Gap 1 below). To stay
checkable end to end, the round and the lens are modelled here as typed
`action`s, and the review fan-out sits after the developer/gate cycle rather
than inside it (Gap 2). Both compromises are annotated in the file's header.

Verification:

```
$ aion awl check dev_brief.awl
ok: dev_brief.awl (8 steps)
```

It is also byte-canonical (`aion awl fmt` is a no-op) and format-idempotent.

| Construct | Rev | Where |
|---|---|---|
| `workflow` / `about` / `input` / `output` / `error` | rev-0 | header |
| `type` (record) with `List(T)`, `Int`, `Bool`, `String` fields | rev-0 | 13 type decls |
| `action` contract `-> Type` | rev-0 | 10 actions |
| action routing: `queue` / `node` / `timeout` / `retry … every` | rev-0 | `provision_workspace`, `fix_round`, `review_lens`, … |
| `step` / `do` / `as` (bind) | rev-0 | every step |
| `as` (rebind) — threaded through `repeat`, and on the guarded `rework` step | rev-0 | `fix_cycle`, `rework` |
| `each <id> in <expr>` (parallel fan-out) | rev-0 | `review` over `config.lenses` |
| `repeat up to <expr>` + `until <expr>` (bounded cycle) | rev-0 | `fix_cycle` |
| `when <expr>` guard with field access + `not` | rev-0 | `rework` (`when not review.accepted`) |
| field access (`config.repo_root`, `round.gates_green`, `review.accepted`) | rev-0 | throughout |
| `on failure` handler → `do` … `fail` | rev-0 | `cleanup` |
| `finish <expr>` | rev-0 | trailer |

---

## B. `sized_fanout.awl` — AWL-1 (aspirational; does not check yet)

The canonical dynamic-width pattern, expanded into a complete document: a
`size_work` action returns `List(Batch)` whose length is unknown until runtime;
a parallel `each` fan-out processes every batch; the results are merged into a
categorized outcome; a `match` on that category routes to a different
completion action per variant (clean → publish, conflicted → reconcile,
diverged → escalate); and a sequential `each … in order` runs an ordered
cleanup pass over the touched regions. The fan-out width is never named in the
language — that is the whole point of the pattern.

rev-0 parts (dynamic `each` fan-out, field access, record model) are
implemented. The AWL-1 parts are the payload-less `enum` declaration, the
exhaustive `match`/`case`, and the `in order` modifier.

Verification (rejected at the first AWL-1 token — the enum declaration):

```
$ aion awl check sized_fanout.awl
sized_fanout.awl:19:1: error: type declaration needs record fields
```

| Construct | Rev | Where |
|---|---|---|
| `workflow` / `about` / `input` / `output` / `error` | rev-0 | header |
| `type` (record), `action` contracts, routing fields | rev-0 | decls |
| `do … as` (bind), `finish` | rev-0 | throughout |
| `each <id> in <expr>` (parallel fan-out, dynamic width) | rev-0 | `process` over `batches` |
| field access (`merge.category`, `merge.regions`) | rev-0 | `complete`, `cleanup` |
| **`enum`**: `type MergeCategory = Clean \| Conflicted \| Diverged` | **AWL-1** | line 19 |
| **`match` / `case`** (exhaustive, no default arm), arms bind same `as` name | **AWL-1** | `complete` |
| **`each … in order`** (sequential iteration) | **AWL-1** | `cleanup` |

---

## C. `approval_gate.awl` — AWL-1 (aspirational; does not check yet)

A human-in-the-loop shape: prepare a spend request, then park on a durable
`wait` gate with a `timeout` and an `on timeout` handler that auto-rejects and
finishes with an expired disposition. An approval applies the decision; the
`otherwise` arm records a rejection (binding the same `receipt`). A `spawn`
fires a fire-and-forget notification child that the gate never waits on, and a
final `on failure` block rolls the ledger entry back before re-raising, so no
half-applied approval is ever left behind.

The durable-gate machinery (`wait` + `timeout` + `on timeout`) and the
`on failure` compensation block are rev-0. The AWL-1 parts are `otherwise` and
`spawn`.

Verification (rejected at the first AWL-1 token — `otherwise`; the rev-0
`wait`/`timeout`/`on timeout` above it parse and check):

```
$ aion awl check approval_gate.awl
approval_gate.awl:60:3: error: unknown step field `otherwise`
```

| Construct | Rev | Where |
|---|---|---|
| `workflow` / `about` / `input` / `output` / `error` / `signal` | rev-0 | header |
| `type` (record), `action` contracts, routing fields | rev-0 | decls |
| `wait <signal>` + `timeout <duration>` | rev-0 | `await_decision` |
| `on timeout` handler → `do` … `finish` | rev-0 | `await_decision` |
| record construction (`Outcome(id: …, disposition: …, detail: …)`) | rev-0 | `on timeout` finish |
| `when <expr>` guard with field access | rev-0 | `approved_path` |
| `on failure` handler → `do` … `fail` | rev-0 | `finalize` |
| `finish <expr>` | rev-0 | trailer |
| **`otherwise`** (complement of the nearest preceding `when` binding the same name) | **AWL-1** | `rejected_path` |
| **`spawn <child>(args)`** (fire-and-forget child, no `as`) | **AWL-1** | `notify` |

---

## Gaps discovered while authoring

### Gap 1 — the spec's canonical bounded-cycle example does not typecheck (child results are opaque)

`AWL-0-SPEC-DRAFT.md` §"Loops" and sketch H both present the bounded fix cycle
as `repeat up to N` / `do child fix_round(state)` / `until state.accepted` /
`as state`. That pattern **fails `aion awl check` today**, because rev-0 child
results are untyped: they cannot be field-accessed, passed to a typed
parameter, or returned. The two committed fixtures that render exactly this
pattern both fail:

```
$ aion awl check crates/aion-awl/tests/fixtures/bounded_cycle.awl
crates/aion-awl/tests/fixtures/bounded_cycle.awl:22:3: error: as binding `state` expected ReviewState, found untyped child result

$ aion awl check crates/aion-awl/tests/fixtures/bounded_cycle.canonical.awl
crates/aion-awl/tests/fixtures/bounded_cycle.canonical.awl:23:3: error: as binding `state` expected ReviewState, found untyped child result
```

(`bounded_cycle.awl` is still exercised by the parser/printer/emitter golden
tests, which do not run the checker — so the discrepancy is invisible in CI.)

The checker's own tests assert this opacity as intended rev-0 behaviour
("child result is untyped in this revision and cannot be field-accessed"). So
the tension is a genuine **spec-vs-implementation discrepancy**, not an authoring
mistake: the flagship bounded-cycle example in the spec requires field access
on a child result, which the implemented rev-0 checker forbids. It also brushes
against ratified ruling #5 ("`until` sees the step's own fresh `as` binding …
must typecheck") — that ruling makes the poll loop legal for a *typed action*
result, but says nothing that would rescue the *child* form the spec actually
renders.

Consequences that shaped the examples:

- `dev_brief.awl` models the fix round and the review lens as typed `action`s
  (not `do child`) so its `repeat`/`until` bound and its adjudication over the
  verdict list actually typecheck. The child-workflow form is the intended
  shape; it is blocked by this gap.
- Any workflow that fans out over child workflows and then *consumes* their
  results (field access, typed pass-through, or a typed `finish`) is currently
  inexpressible in checkable rev-0. `each <id> in <xs>` / `do child f(<id>)` /
  `as ys` binds `ys` to a list of untyped child results with no legal consumer.

Recommendation for Tom's call: either give child results a declared type
(e.g. a `child <name>(params) -> Type` contract, mirroring `action`), or amend
the spec/fixtures so the canonical bounded cycle uses a typed action. Today the
spec and the checker disagree.

### Gap 2 — the one-call-body rule prevents showing `each` fan-out *inside* a bounded cycle in one document

Independent of Gap 1: `each` and `repeat` bodies are "exactly one `do`", so a
bounded cycle whose round is genuinely developer → gate → review-fan-out must
extract that round into a child workflow. Combined with Gap 1's opacity, a
single document therefore cannot exhibit **both** the `each` fan-out over
lenses **and** the `repeat`/`until` that encloses it — the fan-out necessarily
lives in the (separate, and result-opaque) child. `dev_brief.awl` resolves this
by placing the review fan-out after the developer/gate cycle rather than inside
it. This is a modeling constraint the language imposes by design, recorded here
because it changes the shape of any faithful re-expression of dev-brief. No new
syntax was invented to work around it.

### Gap 3 (minor, formatter) — empty comment lines gain trailing whitespace

`aion awl fmt` rewrites an empty comment line `//` to `// ` (a trailing space).
Noticed while canonicalizing `dev_brief.awl`; not a language gap, but the
canonical rendering of a blank comment line carries trailing whitespace, which
some editors and linters strip.
