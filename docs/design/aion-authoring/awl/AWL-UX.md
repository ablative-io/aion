# AWL-UX — the AWL authoring experience: the north star

Status: DIRECTION for implementers (human and AI). This document is the
practical north star for the authoring experience AWL is building toward. It is
written as **acceptance direction**: every statement here is meant to be
concrete and checkable, not marketing. Where a claim describes something the
`aion` binary does today it is marked **SHIPPED**; where it describes the target
it is marked **PLANNED** with its tracker.

Sources of truth this document is subordinate to:

- [AWL-0-SPEC-DRAFT.md](AWL-0-SPEC-DRAFT.md) — the language: design constants,
  grammar, semantics, and the AWL-1 sanctioned rev with Tom's ratified rulings.
- [AWL-BC-DESIGN-DRAFT.md](AWL-BC-DESIGN-DRAFT.md) — §8 (authoring end state)
  and the "one wall of errors" principle.
- `crates/aion-awl/src/checker.rs` — the real diagnostic voice this document's
  error examples must match.

Current-state anchor: **aion 0.8.0**. The AWL surface that ships today is
`aion awl {check,fmt,emit}`. Everything below distinguishes that from the target
so nobody builds against a claim that is not yet true.

Tracker legend used throughout: **#215** `aion run --watch` (the unified
compile-deploy-stream loop); **#240** AWL-BC direct bytecode emission (the
`.awl → .beam` backend, supersedes the interpreter tier #216); **#241** the
AWL-1 language rev (`otherwise`, `match`/enums, `parallel`/`race`, `spawn`,
`each … in order`, literal indexing); **#218** deterministic package bytes;
**#234** the stale-build-cache trap class.

---

## 1. The promise

One page. If AWL delivers nothing else, it delivers these six sentences, and
every implementation decision is measured against them.

1. **A workflow is one plain-text file.** `onboard_customer.awl` is the whole
   artifact — inputs, types, action contracts, and steps in one canonical
   order (AWL-0 §"Document grammar"). No project scaffold, no sidecar YAML, no
   generated file anyone edits. The `.awl` document *is* the source of truth;
   Gleam, canvas, and docs are generated views nobody hand-edits (AWL-0
   §Position).

2. **The only tool is the `aion` binary.** No Gleam, no Erlang/OTP, no `erlc`,
   no package manager, no language server required to author. One binary on
   `PATH` is the entire toolchain (AWL-BC §"What this buys"). *Today the
   author-facing loop `aion awl {check,fmt,emit}` already needs nothing but
   `aion`; the compile-to-run path still needs Gleam until #240 lands — see §2.*

3. **Check-clean means it runs.** The AWL typechecker is the ONLY error
   surface. When `aion awl check` is silent, there is no second compiler to
   appease and no diagnostic will ever point at code the author did not write
   (AWL-BC §"One wall of errors, not two"). *This is guaranteed structurally
   only once emission is total for checked programs (#240); the Gleam stopgap
   can still surface generated-code errors, which is precisely the wound #240
   closes.*

4. **The file narrates itself in the console.** `about` prose is load-bearing,
   not a comment: it flows to the canvas node label, the ops-console narration,
   and generated docs. A running workflow narrates itself in its author's words
   (AWL-0 design constant 4; AWL-BC §8 "reads as a runbook").

5. **Same source → same bytes → same deploy identity.** A given `.awl`
   compiles to the same artifact bit-for-bit, so it packages to the same
   content hash and deploys as the same version every time (AWL-BC §"What this
   buys"; fixes the #218 nondeterminism family for AWL workflows). *PLANNED via
   #240; today's Gleam path does not yet guarantee this for AWL.*

6. **Determinism is unexpressible, not policed.** The language has no
   vocabulary for clock, randomness, or ambient I/O. The only world-touching
   verb is `do` (call an action); time is engine-mediated (`sleep`, `wait` +
   `timeout`). An author — human or AI — cannot write a replay-unsafe workflow
   even by accident (AWL-0 design constant 1 and §Semantics "Determinism").

---

## 2. The human authoring loop

A concrete end-to-end session. Paths and names are realistic; every command
carries its status. Where the target loop differs from what 0.8.0 ships, the
shipped equivalent is spelled out so nobody writes acceptance tests against a
command that does not exist yet.

### 2.1 Write the file

```
$ $EDITOR onboard_customer.awl
```

One file. `about` lines are prose, not comments — they will show up in the
console and on the canvas, so they are written for the operator who reads them
during an incident, not for the compiler.

```awl
workflow onboard_customer
about Onboard a new customer: verify the signup, provision the account, welcome them.

input signup: Signup
output Account

type Signup { email: String, plan: String, seats: Int }
type Account { id: String, active: Bool }

action verify(email: String) -> Bool
action provision(plan: String, seats: Int) -> Account
action welcome(account: Account) -> Nil

step verify
  about Confirm the email is reachable before we spend anything provisioning.
  do verify(signup.email)
  as ok

step provision
  about Stand up the account for the chosen plan and seat count.
  do provision(signup.plan, signup.seats)
  retry 3 every 30s
  as account

step welcome
  about Send the welcome sequence; best-effort, never blocks activation.
  do welcome(account)

finish account
```

### 2.2 Check — and get a real diagnostic (SHIPPED)

The author fat-fingers the first argument and passes `signup.seats` (an `Int`)
where `verify` wants the `email` (a `String`):

```
$ aion awl check onboard_customer.awl
onboard_customer.awl:16:6: error: argument `email` for action `verify` expected String, found Int
```

`<file>:<line>:<column>: error: <message>` to stderr, exit non-zero
(`aion awl --help`). This is the exact voice of `checker.rs`
(`CheckError { span, message }`) — see §4 for the full quality bar. Fix it back
to `do verify(signup.email)`.

### 2.3 Check clean (SHIPPED)

```
$ aion awl check onboard_customer.awl
ok: onboard_customer.awl (3 steps)
```

Silent-but-for-the-summary. Check-clean is the gate: it means the document
parses, every action call matches its declared contract, every binding
resolves, and every step field is well-formed.

### 2.4 Format (SHIPPED)

```
$ aion awl fmt onboard_customer.awl
formatted: onboard_customer.awl
```

`fmt` rewrites the file in place with the canonical printer — there is exactly
one true rendering, so there is nothing to argue about in review and diffs stay
minimal (which matters most for AI authors, §3). The printer *is* the
formatter; there is no separate check-mode (`aion awl --help`). Property:
`parse ∘ print = id` (AWL-0 §"What AWL-0 builds").

### 2.5 Run it, watch it (PLANNED — #215)

The target single-command loop compiles in milliseconds, deploys to the dev
server, streams the run, and re-runs on save:

```
$ aion run onboard_customer.awl --input @cust.json --watch      # PLANNED #215
   compiled onboard_customer.awl → onboard_customer@3f9c1a (4 ms)
   deployed to 127.0.0.1:50051 (version 3f9c1a)
   run r-0192aa started
   ▸ verify      ok = true                       (0.3s)
   ▸ provision   account = Account(id: "acct_5H…", active: true)   (2.1s)
   ▸ welcome     …                               (0.1s)
   ✓ finished    Account(id: "acct_5H…", active: true)
   watching onboard_customer.awl — edit to re-run
```

This is the loop named in AWL-BC §8. It does not exist in 0.8.0.

**Shipped equivalent today**, in steps, for a Gleam-backed project:

```
$ aion awl emit onboard_customer.awl -o src/onboard_customer.gleam   # SHIPPED (Gleam stopgap)
$ aion dev --gleam-path $(which gleam) .                             # SHIPPED: watch + rebuild + hot-load on save
$ aion input onboard_customer > cust.json                            # SHIPPED: valid input skeleton from the type
$ aion start onboard_customer --input-file cust.json                 # SHIPPED: start a run
$ aion describe onboard_customer                                     # SHIPPED: history of the latest run
```

Note the honest gap: `aion dev` watches a **Gleam project** (`gleam.toml` +
`workflow.toml`), rebuilds through the external `gleam` binary
(`--gleam-path` is required, no default — ADR-001), and hot-loads with no
engine restart. `#215`'s promise is to collapse emit + build + package +
deploy + start + stream into `aion run <file>.awl --watch` with no Gleam stage
at all — which is only possible once #240 removes the toolchain from the path
(§2.7). Until then, the milliseconds-not-toolchains property (AWL-BC §"What
this buys") is aspirational for the run loop.

### 2.6 Watch it in the ops console (see §5 for the full picture)

`aion describe` / `aion inspect` / `aion query` are the shipped observability
surface (`aion --help`). The live-narration ops console that renders `about`
prose as canvas labels and streaming narration, plus driven-mode intervention,
is design intent detailed in §5.

### 2.7 Package and deploy — same bytes every time

Target:

```
$ aion package && aion deploy onboard_customer.aion      # PLANNED: .awl → .beam natively (#240, AWL-BC BC-5)
```

Under #240, `aion package` lowers `.awl → .beam` in-process, deterministically,
so two packages of the same source produce an identical content hash (AWL-BC
§"What this buys"; #218). **Shipped today**: `aion package` packages an
already-built **Gleam** project into `.aion` archives (`--build` runs
`gleam build` first), and `aion deploy <archive>` loads it into a running
server (`aion --help`). The AWL-native, deterministic-bytes package is PLANNED.

---

## 3. The AI authoring loop

AI agents are the primary authors (AWL-0 design constant 5). The whole language
is shaped around that fact, and this section is the acceptance direction for any
pipeline that has an agent emit workflows.

### 3.1 Why the loop is trivial to automate

- **The grammar fits in a system prompt.** The reserved-keyword inventory is a
  single table (AWL-0 §"Reserved keywords"); the document grammar is one code
  block; the step fields are one table; the expression keel is deliberately a
  micro-grammar (references, field access, calls only in `do`, record
  construction, list literals, `not`/`and`/`or`, comparisons, string `+` — and
  nothing else). An agent can hold the entire language in context and the
  language does **not** resemble any existing one, so it cannot pull
  plausible-but-wrong priors from a lookalike syntax (design constant 5).

- **One wall of errors.** The loop is: **write → `aion awl check` → mechanical
  repair from the span diagnostics → done.** There is no second compiler wall
  and — once #240 lands — no diagnostic that points at generated code the agent
  never wrote (AWL-BC §"One wall of errors"). `awl check` is a
  machine-checkable gate a pipeline loops on: exit 0 and the artifact is
  finished; exit 1 and each line is `file:line:col: error: message`, trivially
  parsed and mapped back onto the source span to repair.

- **Determinism is unexpressible.** The agent cannot emit a clock/random/IO
  call to fix a problem because no such vocabulary exists to emit (design
  constant 1). This removes an entire class of "the AI wrote something
  replay-unsafe" failure — not by linting it after the fact, but by leaving no
  way to say it.

- **Schemas fall out of the same declarations.** The `type`, `input`, and
  `output` declarations are JSON-schema derivable, so the start form, the
  structured-output shape, and the worker contract all derive from the one
  place the agent already wrote (AWL-0 §"Document grammar": "the start form,
  structured-output authoring, and the worker contract all fall out of the same
  declaration"). The agent never describes the same shape three times; it
  declares each `type` once. (`aion input <type>` already demonstrates the
  skeleton-from-schema derivation on the Gleam side — SHIPPED.)

### 3.2 A realistic agent transcript

Prompt to the authoring agent (the language spec is in its system prompt):

> Author `triage_ticket.awl`. Input is a `Ticket { id: String, body: String }`.
> Classify it with the `classify` action into a `Category`, then handle it:
> urgent tickets page on-call, routine tickets get enqueued. Output a
> `Handled { by: String }`.

Agent's first emission:

```awl
workflow triage_ticket
about Triage an inbound ticket: classify it, then route to the right handler.

input ticket: Ticket
output Handled

type Ticket { id: String, body: String }
type Triage { category: String, urgent: Bool }
type Handled { by: String }

action classify(body: String) -> Triage
action page_oncall(id: String) -> Handled
action enqueue(id: String) -> Handled

step classify
  about Read the ticket and decide how hot it is.
  do classify(ticket.body)
  as triage

step page
  about Urgent tickets wake a human immediately.
  when triage.urgent
  do page_oncall(triage.id)
  as handled

step enqueue
  about Everything else joins the normal queue.
  otherwise
  do enqueue(ticket.id)
  as handled

finish handled
```

`aion awl check triage_ticket.awl`:

```
triage_ticket.awl:18:6: error: type `Triage` has no field `id`
```

The agent reads the span (`page_oncall(triage.id)` — it referenced the wrong
record) and repairs mechanically: `page_oncall(ticket.id)`. Re-check:

```
triage_ticket.awl:22:3: error: `otherwise` requires a preceding `when`-guarded step binding the same name
```

*(PLANNED voice — `otherwise` is AWL-1, #241; the enforcement rule is spec'd in
AWL-0 §"Branching".)* The agent's `page` step does bind `handled` under a
`when`, so this passes once #241 ships; on 0.8.0 the agent would instead express
the either/or with a second `when not triage.urgent` guard (the rev-0 shape the
`otherwise` rule completes). Either way the loop is the same: check, read the
span, repair, re-check until clean. When check is clean the artifact is done —
there is no second gate.

The load-bearing property for pipelines: **the agent never needs to run the
workflow to know it will compile.** `awl check` clean is the contract.

---

## 4. The error experience

The quality bar. Every diagnostic is span-anchored and reads like a compiler,
not a validator (AWL-0 design constant 3). The format is fixed by
`aion awl`: `<file>:<line>:<column>: error: <message>`, one per line, to
stderr, non-zero exit. The message voice is fixed by `checker.rs`: it names the
offending thing in backticks and, for type errors, always says
`… expected <X>, found <Y>`.

The four scenarios the task calls out, plus two more that set the bar. The
SHIPPED examples below are **verbatim output from `aion awl check` on
aion 0.8.0.**

### 4.1 Type mismatch on an action call (SHIPPED)

Passing an `Int` where the action's contract declares a `String`:

```awl
action provision(seats: String) -> Account
step provision
  do provision(signup.seats)      // signup.seats : Int
  as account
```

```
onboard_customer.awl:9:16: error: argument `seats` for action `provision` expected String, found Int
```

The span points at the argument expression; the message names the parameter,
the action, and both types. (`checker.rs::call_ty` → `expect_type`.)

### 4.2 Unknown binding (SHIPPED)

Referencing a name that was never bound (typo of `verified`):

```awl
step provision
  do provision(signup.plan)
  when verifid                    // no such binding
  as account
```

```
onboard_customer.awl:19:8: error: unresolved reference `verifid`
```

The checker resolves every reference against inputs and prior `as` bindings;
an unresolved one is named at its span. (`checker.rs::expr_ty` → `Expr::Ref`.)

### 4.3 `each` over a non-list (SHIPPED)

Fanning out over a `String` instead of a `List(T)`:

```awl
step provision
  each e in signup.email          // signup.email : String
  do provision(e)
  as accounts
```

```
c.awl:9:13: error: each expression expected List(T), found String
c.awl:10:16: error: unresolved reference `e`
c.awl:12:8: error: finish expression expected Account, found List(Account)
```

Note the honest cascade: when the iteration expression is not a list, the loop
binding `e` never comes into scope, so the downstream references also fault.
This is real checker behavior (`checker.rs::check_step`, the `each` arm) — the
first error is the root cause; implementers should not "fix" the cascade by
suppressing follow-on errors, because each one is independently true and points
at real work.

### 4.4 Non-exhaustive `match` (PLANNED — AWL-1, #241)

`match` is exhaustive by construction: there is no default arm, on purpose, so
that adding an enum variant breaks every workflow that fails to route it
(AWL-0 §"Branching"). The diagnostic must land in the established voice —
model it on the checker's existing "missing field" message
(`checker.rs::record_ty`: `missing field \`{f}\` for record \`{name}\``):

```awl
type Category = Urgent | Routine | Spam
step route
  match ticket.category
  case Urgent
    do page_oncall(ticket.id) as handled
  case Routine
    do enqueue(ticket.id) as handled
  // Spam not handled
  as handled
```

Target diagnostic:

```
triage.awl:12:3: error: match on `Category` is not exhaustive: missing case `Spam`
```

Acceptance note for the #241 implementer: the span is the `match` keyword; the
message names the enum type and every unrouted variant. This surface does not
exist in 0.8.0 (`type Category = …` is currently a parse error:
`type declaration needs record fields`).

### 4.5 Bonus quality-bar examples (SHIPPED)

These are not in the task's list but they set the bar for the rest of the
checker and are verbatim 0.8.0 output.

Unknown action (typo of `provision`):

```
b.awl:9:6: error: unknown action `privision`
```

Wrong argument count against the contract:

```
e.awl:9:6: error: action `provision` expected 2 argument(s), found 1
```

Field access on a non-record:

```
d.awl:9:16: error: field access expected record type, found String
```

Non-`Bool` `when` guard:

```
f.awl:9:8: error: when guard expected Bool, found String
```

The through-line for implementers: no error is ever generic ("invalid
workflow"), none points at a line the author did not write, and every type
error carries `expected X, found Y`. That is the bar.

---

## 5. What the operator sees

The runbook property (AWL-0 design constant 4; AWL-BC §8 "reads as a runbook").
The artifact the author writes is the artifact the operator reads during an
incident, because `about` prose is the same string in three places.

### 5.1 `about` prose is the operator's text

Each step's `about` line becomes:

- the **canvas node label** for that step,
- the **live console narration** as the step runs,
- the corresponding line in generated docs.

So from the §2 workflow, a running `provision` step narrates itself as
*"Stand up the account for the chosen plan and seat count."* — the author's own
words, not `step: provision (activity onboard_customer.provision) RUNNING`. A
running workflow narrates itself; the operator watching an incident reads the
intent, not the wiring.

Target ops-console view of a live run:

```
onboard_customer  ·  run r-0192aa  ·  RUNNING
  ✓ verify     Confirm the email is reachable before we spend anything provisioning.   ok=true   0.3s
  ▸ provision  Stand up the account for the chosen plan and seat count.                 retry 1/3  2.1s
    welcome    Send the welcome sequence; best-effort, never blocks activation.         pending
```

### 5.2 Driven mode and message injection (PLANNED)

The ops console is the place an operator intervenes in a live run: inject a
signal into a durable gate, or step a run in driven mode and inspect it between
steps. A durable `wait review` gate (as in the `research_report` fixture) is
exactly the injection point — the operator supplies the `Approval` the workflow
is parked on.

Shipped substrate today (`aion --help`): `aion signal` (send a signal to the
latest run), `aion query` (query the latest run), `aion pause`/`aion resume`
(durably hold/release dispatch), `aion describe` (history), and `aion inspect`
(time-travel over a recorded run's event-store oplog). The live-narration
canvas that renders `about` prose, and driven-mode transcripts with in-console
message injection, are the design end state built on that substrate — mark them
PLANNED. The acceptance direction: an operator should be able to read, narrate,
pause, inject into, and replay a run entirely in the workflow author's own
prose, never in generated identifiers.

---

## 6. Anti-goals

What AWL authoring must NEVER become. Each of these is a hard line, not a
preference; an implementer who finds themselves crossing one has taken a wrong
turn.

1. **No YAML, no YAML-like, no markdown authoring surface.** Tom ruled
   (2026-07-04) YAML-likes and markdown are dead as authoring surfaces
   (markdown = generated docs only) — AWL-0 §Position. If a design reaches for
   a config-file shape, it has failed.

2. **No second source of truth.** The `.awl` document is the source; Gleam,
   canvas, bytecode, schemas, and docs are all generated views, and **nobody
   ever edits generated output** (AWL-0 §Position). The invariant was never
   "no DSL" — it was "no second source of truth", and AWL keeps it (AWL-0
   §Position).

3. **No toolchain installs to author.** Authoring must require exactly one
   binary: `aion`. No Gleam, no OTP, no `erlc`, no package fetch, no build
   cache to go stale (AWL-BC §"What this buys"; this is what #240 buys and what
   shrinks the #234 trap class). Any proposal that adds a required install to
   the author's path is an anti-goal.

4. **No generated-code errors leaking to the author.** Check-clean means it
   runs; the AWL typechecker is the only error surface. A diagnostic that
   points at emitted Gleam or emitted `.beam` — code the author never wrote — is
   the exact wound AWL-BC closes (§"One wall of errors"). The Gleam stopgap can
   still do this today; that it can is the bug, not the baseline.

5. **No lookalike syntax priors.** The grammar must not resemble an existing
   language, because lookalike syntaxes hand LLMs plausible-but-wrong priors
   (AWL-0 design constant 5). Do not "borrow" Python/JS/HCL/Rust surface
   syntax to feel familiar — familiarity here is a defect.

6. **No computation vocabulary — plumbing yes, computation no.** This is the
   spec's design line, cited exactly: *"AWL must be sufficient to author a
   complete workflow alone — schemas, routing data between steps, selecting
   fields and elements — without reaching for Gleam. What stays out is
   computation: folds, string manipulation, arithmetic-heavy transforms live in
   actions behind the contract boundary"* (AWL-0 §"Design line: plumbing yes,
   computation no"). Plumbing — `type` decls, field access, record
   construction, list literals, `each` over any list, and (AWL-1) enums,
   `match`, literal indexing — is in. A `sum`/`filter`/`regex`/arithmetic
   vocabulary is out. No `now()`/`random()` (AWL-0 open decision 3); no
   arithmetic beyond string `+` (open decision 4). Real computation lives in
   actions behind the contract boundary — always.

7. **No silent defaults on unratified values.** Where the spec defers a value
   to Tom (the continue-as-new history threshold, AWL-1 ruling 3; the
   sequential-each spelling before it was ratified), no "sensible default"
   ships. This mirrors the project standard: no assumed defaults.

---

## Appendix: status ledger

| Surface | Status | Notes |
|---|---|---|
| `aion awl check` | SHIPPED | span-anchored diagnostics; `ok: <file> (N steps)` on success |
| `aion awl fmt` | SHIPPED | canonical printer, in-place; `formatted: <file>` |
| `aion awl emit` (→ Gleam) | SHIPPED | the stopgap backend; typecheck-gated |
| `aion dev` (Gleam project watch) | SHIPPED | needs `--gleam-path`; rebuilds via `gleam`, hot-loads |
| `aion package` / `aion deploy` (Gleam) | SHIPPED | operates on a built Gleam project / `.aion` archive |
| `aion input` / `aion start` / `aion describe` / `aion inspect` / `aion signal` / `aion query` / `aion pause` / `aion resume` | SHIPPED | observability + intervention substrate |
| `aion run <file>.awl --watch` | PLANNED | #215 — unified compile→deploy→stream loop, no Gleam stage |
| `.awl → .beam` native, deterministic bytes | PLANNED | #240 (AWL-BC); fixes #218 for AWL, closes the two-walls wound |
| AWL-1 language (`otherwise`, `match`/enums, `parallel`/`race`, `spawn`, `each … in order`, literal indexing) | PLANNED | #241 |
| Live-narration ops console + driven-mode injection | PLANNED | design end state on the shipped observability substrate |
