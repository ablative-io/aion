<!-- STATUS: DIRECTION CAPTURE (2026-07-17), from Tom's rulings in DM that morning.
Not yet a build-approved design — it records the agreed direction, the taxonomy, the
language model for AWL-authored workers, and the recommended sequence, so the next
design pass starts from the paper of record rather than a channel transcript.
Companion to WORKER-DEPLOYMENT.md (the deployment/placement/supervision machinery,
2026-07-01 draft): that doc covers how worker artifacts get placed, supervised, and
drained; THIS doc covers what workers ARE, how they are authored, and what ships in
the box. The two compose: everything authored here deploys through that machinery. -->

# Worker Authoring Story — three layers, batteries included, and AWL on both sides of the queue

> Owner's rulings this doc records (Tom, 2026-07-17):
> 1. **No arbitrary boundaries on AWL.** It will probably never be a general-purpose
>    language, but we do not pre-draw the line. Growth is *problem-driven*: when a
>    real problem appears (data wrangling, templating, command execution), we ask
>    "can we put this in the language?" and add the vocabulary properly.
> 2. **Namespaced word-sets.** Worker-execution vocabulary is not usable in a
>    workflow file, and vice versa. The file declares what it is; the checker
>    enforces which words exist there.
> 3. **Keep moving.** This work continues; capture everything, do not stall it.
> 4. **Elixir support on beamr is a goal** (community size; pipe syntax feels
>    well-suited to this domain).

---

## 1. The three worker layers (taxonomy)

Every worker anyone will ever write for aion falls into one of three layers. The
layers differ in *authoring effort and runtime posture*, not in protocol — all
three register and serve through the same task-queue machinery.

| Layer | What it is | Authoring surface | Status today |
|---|---|---|---|
| **1 — Command runner** | Each action runs a command; result is text or JSON | Manifest (exists), AWL worker file (planned §4) | **EXISTS**: `aion worker shell --manifest` (`[worker]` + `[[action]]` command, `result = text\|json`) — it served the live `ask_once` demo |
| **2 — Script worker** | Single file doing data handling / processing (e.g. a Python script with decorated action functions) | Python SDK + script host (§3), AWL worker file for the declarative subset (§4) | NOT BUILT — the Python image-recognition examples are the first target |
| **3 — Service worker** | Substantial long-lived processing: endpoints, payments, anything with real logic and state | Full SDKs (Gleam/Rust today; Python/TS to come) | EXISTS (Gleam/Rust SDK); more SDK languages are roadmap |

The insight the taxonomy buys: **layer 1 already exists** — what is missing around
it is *lifecycle* (server-owned deploy/start/stop/supervise) and *authoring* (create
one from the studio without leaving the surface). Those two gaps, not new worker
kinds, are the real work.

## 2. Batteries included: what ships with the server

Same philosophy as the embedded ops console: the complete experience is the
default. The server ships with:

1. **The shell worker** (exists) — layer 1 out of the box.
2. **An ACP worker** — one worker speaking the Agent Client Protocol, which makes
   *any* ACP-speaking agent harness (Claude Code, Codex, OpenCode, Pi, …) an
   activity provider with zero per-tool adapters. This is the multi-harness worker
   direction decided weeks ago, landed as a shipped battery rather than an
   integration exercise.
3. **A Python script host** — runs a single-file Python worker (decorator per
   action). The thin Python SDK underneath it is also usable standalone (layer 3
   in Python).
4. **The existing SDK story** (Gleam/Rust), kept excellent.

Worker *lifecycle through the server* is the linchpin for all of these: deploy from
a library, start/stop/restart, supervision — Aion-native (launchd is ruled out,
2026-07-14), drivable from the console. The machinery design is
[WORKER-DEPLOYMENT.md](WORKER-DEPLOYMENT.md) (WD-0..5); this doc adds the demand
side: every battery above needs somewhere to live, so lifecycle lands first.

## 3. The Python story

- **Thin SDK**: decorate a function per action; the SDK handles registration,
  heartbeat, payload codec. Small enough to read in one sitting.
- **Script host**: `aion worker python script.py` (shape TBD) — the host owns the
  process lifecycle so a bare script is a deployable worker.
- **First demos**: Tom's image-recognition examples — real media through an aion
  pipeline, results back, watched live in the console. This is deliberately the
  most *visceral* proof of the pipeline story and should be built as demo-first.

## 4. AWL-declared workers — the language model

### 4.1 File kinds and namespaced vocabularies (Tom's ruling)

An `.awl` file declares what it is at the top: a **workflow** (today's only kind)
or a **worker**. The declaration selects the file's *word-set*:

- **Common core** (both kinds): types, values, expressions, literals, comments,
  the `worker X with actions …` *interface* declaration.
- **Workflow words**: steps, `distribute`/`collect`, `child`, `visits`, routes,
  `on failure`, … — everything that means *durable orchestration*.
- **Worker words**: action *bodies* — command execution, templating, parsing,
  querying/filtering — everything that means *doing the work*.

The checker enforces the boundary: workflow vocabulary in a worker file (or vice
versa) is a check error with a pointed message. This is the honest version of
"don't become a general-purpose language": the vocabulary is not capped, it is
**scoped** — every word means exactly what it says *in the place it is allowed*,
which is the hard law applied to language design. It also makes the (not yet
built) LSP autocomplete kind-aware from day one.

### 4.2 Candidate worker vocabulary (grow problem-driven, not all at once)

In rough order of demonstrated need:

1. **Command execution** — lift the shell-worker manifest shape into the language:
   command, args, stdin, timeout, `result = text|json`. Semantics already proven
   by the existing shell worker; this is syntax over a working engine.
2. **Templating** — string templates with typed holes (workflow inputs/outputs are
   already typed; the holes check).
3. **Data wrangling: query / search / filter** — a typed pipeline vocabulary over
   JSON-shaped values (think a small, typed jq). Tom's pipe instinct applies:
   pipeline syntax fits this domain naturally, and we control the grammar.
4. **Parsing** — structured extraction from text/JSON into declared types.

Each addition rides the full toolchain (lexer → parser → checker → emitter → LSP →
tree-sitter grammar) — the pipeline that has now shipped six times. Nothing lands
as a stub: a word that exists, works.

### 4.3 The escape hatch and the long arc

- **Escape hatch**: when logic outgrows the vocabulary, the studio scaffolds the
  worker into Python or Gleam instead. Growing AWL and keeping native-language
  workers first-class are both true; neither blocks the other.
- **Long arc**: worker-AWL compiles through the same direct MIR→BEAM path as
  workflows (once the remaining direct-path refusals close), and the beamr
  tree-shaking AOT north-star (beamr/docs/AOT-NORTH-STAR.md) then turns worker
  files into tiny native binaries — stable, fast, high-concurrency workers from a
  file you wrote in the authoring studio. Same language on both sides of the task
  queue, bare metal at the end. Nobody in the Temporal/Restate/Inngest quadrant
  has this.

### 4.4 Scaffold-from-studio UX

You are writing a workflow; you reference a worker that does not exist. The studio
offers to scaffold it — as an AWL worker file (layer 1/2 declarative) or a
Python/Gleam template (layer 2/3) — and deploying it lands under server-owned
lifecycle without leaving the surface.

## 5. Elixir on beamr (goal capture)

Tom's ruling: anything that better supports Elixir is worth doing — the community
is large, and Elixir's scripting-like ergonomics + pipe syntax suit this domain.

Two distinct things, kept honest:
- **(a) beamr running Elixir-compiled .beam** — the enabler. Prior sizing (June
  2026 findings, still indicative): reaching Erlang-stdlib coverage ≈ 1–2
  dev-weeks of additive BIFs; Elixir-the-language ≈ 1–3 months; Elixir+OTP
  fidelity multi-month. `beamr imports` + demand-driven BIFs (beamr ADR-006) make
  the BIF work an automatable worklist.
- **(b) Elixir as a worker authoring language** — falls out of (a) plus the layer
  2/3 SDK story; no aion-side design needed beyond the SDK surface.

This is a **beamr roadmap item**, boarded here so it is not lost; it does not gate
any phase of the worker story above. (Pipe syntax in AWL itself, §4.2, needs no
Elixir — we own the grammar.)

## 6. Recommended sequence

1. **The staged dev_brief run** (Tom's Start) — proves the whole authoring→run
   pipeline on real work.
2. **Finish direct-path refusals** (#7's committed finish line: subflow, on
   failure, substeps, value route payloads, dependency-parallel layers) — Gleam
   emitter becomes the exception.
3. **Worker lifecycle through the server** (WORKER-DEPLOYMENT.md WD-phases) — the
   linchpin; everything in §2 needs it.
4. **Python SDK + script host** — the image-example demos.
5. **ACP worker shipped by default.**
6. **Worker-AWL** (§4 — needs 2's compiler + 3's lifecycle), then the AOT arc.

Parallel small lane anytime: LSP autocomplete (verified 2026-07-17: hover,
goto-definition, and formatting exist; no completion provider does) + the editor
punch list (scrubber continuity, Tab indent).

## 7. Open questions for Tom

1. **First worker-vocabulary slice**: recommend **command execution + templating**
   first (§4.2 items 1–2) — dev pipelines need them immediately and the shell
   worker gives the semantics for free. Query/filter/parse follow. Confirm or
   reorder.
2. **Pipe syntax**: adopt a pipeline operator in AWL for the data-wrangling
   vocabulary (and possibly workflow value-shaping)? Recommend yes, designed once,
   in the vocabulary-round-2 design pass alongside predicated distribute (#23),
   signals (#24), and concurrency caps.
3. **ACP worker scope for v1**: activities = "run this agent with this prompt and
   these files, return the result"? Or richer (sessions, tool permissions,
   interventions)? Recommend starting with the stateless activity shape.
4. **Elixir sizing**: board (a) as a beamr milestone now, or after the worker
   story lands? Recommend after — it multiplies audience, not capability.
