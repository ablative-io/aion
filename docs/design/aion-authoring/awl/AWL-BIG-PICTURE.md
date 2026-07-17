<!-- STATUS: DIRECTION MAP (2026-07-17), from Tom's rulings that morning. This is
deliberately NOT a spec. Over-designing the language up front was named by the owner
as the real risk, so far avoided; this doc captures the bigger picture — identity,
thesis, opportunities — without specifying mechanisms. Specs stay where they are
(AWL-2-SPEC.md is the language of record); vocabulary grows problem-by-problem. -->

# AWL, the Big Picture — the Aion Work Language

> Tom's reframe, ruled 2026-07-17: **AWL is the Aion *Work* Language, not a
> workflow language — "a language for getting work done."** Workflows were the
> first kind of work it describes. Workers are the second. The name now means
> what the language is becoming.

---

## 1. The thesis

There are today two dominant ways to define a real multi-step process:

1. **Write it in YAML/JSON/DSL-config** — checkable only in shape, not meaning;
   the pathologies are industry-famous (CI YAML that fails at runtime for reasons
   a compiler would have caught).
2. **Write it in prose/markdown and hand it to an agent** — the agent-era default.
   You document the procedure, the LLM follows it, and the guarantee mechanism is
   *hope*. If you made a mistake, nothing shows you. If the model rolls the dice
   differently today, you find out in production. It is a cheap, flimsy,
   non-deterministic version of the thing you meant.

AWL's thesis is the third way: **real work, defined in a small language you can
write yourself, where the checker shows you your mistake before anything runs,
and the runtime executes exactly what you wrote — durably, resumably, visibly.**
The feeling that matters (Tom, verbatim in spirit): *"I can write it myself and
solve really complex problems without sacrificing anything."* Not a toy, not a
config format, not a dice roll. Every construct that exists, works; every word
means what it says (the hard law, 2026-07-16).

Agents still fit — they *perform* activities (the ACP worker makes any agent
harness an activity provider). The difference is where the definition of the work
lives: in a checked language, not in the prompt.

## 2. What already exists (so the picture stays honest)

The full toolchain has shipped and is live on production:

- **Language**: types, values, expressions; workflows with steps, routes,
  `on failure`, substeps; worker *interface* declarations; the flow vocabulary —
  `distribute`/`collect` (true parallel children), `visits` bounds, `child`
  workflows, loops/forks — all live end-to-end (flow-vocab landings 1–6).
- **Toolchain**: lexer → parser → canonical printer → typechecker → two emitters
  (Gleam pipeline; direct MIR→BEAM with byte-matching identities for the
  vocabulary it accepts and typed refusals for the rest) → packager → deploy.
- **One-motion**: `aion deploy FILE.awl` / `aion run FILE.awl` — file to running
  durable workflow in one command.
- **Surface**: the authoring studio (author → check → canvas → deploy → run →
  observe/scrub), LSP (hover, go-to-definition, formatting), current tree-sitter
  grammar. Missing and known: autocomplete (#31).
- **Runtime**: durable execution with event-sourced history, replay, failover on
  the beamr/haematite/liminal substrate.

Direction papers that this map sits over: [WORKER-AUTHORING-STORY.md](../../WORKER-AUTHORING-STORY.md)
(file kinds, namespaced vocabularies, batteries-included workers),
[WORKER-DEPLOYMENT.md](../../WORKER-DEPLOYMENT.md) (placement/supervision
machinery), AWL-2-SPEC.md (language of record), AWL-FLOW-VOCABULARY.md.

## 3. Two kinds of work, one language

A file declares what it is — a **workflow** (orchestration: durable, replayed,
suspended) or a **worker** (performance: long-lived, concurrent, doing). The
declaration selects its word-set; the checker refuses vocabulary outside its
kind. Growth is problem-driven — command execution, templating, query/filter,
parsing are the named next candidates — and nothing lands as a half-working word.
(Model and sequence: WORKER-AUTHORING-STORY.md §4.)

## 4. The opportunity Tom named: check absolutely everything

**Cross-boundary type checking** — one contract, both sides, before anything
runs:

- The workflow side already checks: a workflow that calls an action with the
  wrong shape is a check error today, against the declared worker interface.
- The missing half is the worker side. For AWL-authored workers it is trivial —
  same compiler, same declarations, the boundary checks itself. For external
  workers (Python, Gleam, ACP) the same interface becomes a **conformance gate at
  deploy/registration time**: a worker that does not satisfy the declared actions
  is refused at the door with a typed error, not discovered mid-run.

End state: **if the system checks, every shape in it is right from the get-go** —
across the orchestration boundary, across languages, before deployment. Markdown
can't do this; YAML can't do this; none of the durable-execution competitors do
it end-to-end (their typed SDKs stop at the language boundary). Owning the
language, the contract format, and the registration path is what makes it
reachable for us.

## 5. Supervised work: the resilience story

Workers become BEAM processes under real supervision (the worker-deployment
design). For command-runner workers this is quietly a big deal: **most real-world
work is shelling out to other tools**, and today the industry's answer to a
crashed runner is a cron job and a prayer. Ours is the same supervision tree that
protects everything else — restart, backoff, liveness, drain — wrapped around
every external command. The whole system's robustness becomes uniform: there is
no unsupervised corner.

The honest risks, named now so they get designed rather than discovered:

- **Retry vs side effects**: a command that charges a card cannot be blindly
  re-run. Idempotence/retry posture must be *declared* per action (the language
  can make this a required word, not a footnote), never assumed.
- **Isolation and resource limits**: BEAM-process isolation is fault isolation,
  not a sandbox for hostile code; hard CPU/mem caps need the OS-process driver
  tier (WORKER-DEPLOYMENT.md §3/§5 keeps this honest).
- **Process hygiene**: orphaned children of killed commands, zombie reaping,
  timeout kill-trees — table stakes for a command runner that claims supervision.

## 6. What owning the whole stack affords (the opportunity list)

We own the language, the compiler, the VM (beamr), and the durable runtime. Each
of these is individually common; owning all four is rare, and it is where the
"otherwise unavailable options" live. Candidates — captured, not committed:

1. **Tree-shaking AOT to native workers** (beamr/docs/AOT-NORTH-STAR.md): a
   worker file compiles to a tiny, stable, high-concurrency native binary. The
   studio-to-bare-metal pipeline nobody else has.
2. **Deterministic replay as a language property**: the compiler knows which
   constructs are replay-safe because we define both the constructs and the
   replayer. Determinism by construction, not by convention-and-code-review.
3. **Cross-boundary contracts** (§4) — the whole-system check.
4. **Hot upgrade tied to content-addressed versioning**: deploys are already
   content-hashed; beamr already does hot code loading; the language can make
   version transitions (drain old, route new) a first-class, checked notion.
5. **Time-travel debugging over durable history**: the scrubber already walks a
   run's history; the language layer can map that history back to source
   positions — scrub a run and watch the cursor move through the .awl file.
6. **Capability-scoped actions**: because the language declares what an action
   does (runs a command, calls a network endpoint), grants can be checked and
   enforced at the boundary rather than trusted.
7. **Kind-aware tooling for free**: namespaced word-sets make autocomplete,
   diagnostics, and the canvas all smarter with zero heuristics.

A research lane (norn, dispatched 2026-07-17) is surveying prior art for each of
these — Ballerina/choreographic programming/session types for §4, Unison/Elm/
Darklang/Erlang-OTP for §6 — plus the negative cases (CI YAML, markdown-for-LLM
procedures). Its findings fold back into this doc; the ranked shortlist it
returns seeds the next design conversation.

## 7. Anti-goals (the discipline that got us here)

- **No up-front over-design.** This document is a map, not a spec. Vocabulary is
  added when a real problem demands it, one word at a time, through the full
  toolchain (lexer → parser → checker → emitter → LSP → grammar), which has now
  shipped six times without a stub.
- **No word that lies.** A construct whose name promises a behavior does that
  behavior — compile-time, run-time, and under failure (the hard law).
- **No general-purpose ambition.** AWL grows toward *work* — orchestration and
  performance of it — not toward being a systems language. When logic outgrows
  the vocabulary, the studio scaffolds a Python/Gleam worker; that is success,
  not failure.
