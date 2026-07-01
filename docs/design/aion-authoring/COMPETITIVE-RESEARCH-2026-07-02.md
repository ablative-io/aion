---
type: research
cluster: aion-authoring
title: Competitive research — workflow-engine authoring & DX (2026-07-02)
provenance: 11-agent parallel research sweep (Temporal, Cadence, Restate, DBOS, Inngest, Trigger.dev, Golem, Azure Durable Functions, Step Functions, Camunda/Zeebe, Prefect, Dagster, Airflow, n8n, Windmill, Node-RED, LangGraph, Mastra, Inngest AgentKit, Protobuf, Cap'n Proto, tRPC, Encore.ts, Effect Schema, Smithy, serde, Gleam-JSON) → landscape synthesis → Aion proposal
raw: ./competitive-research-raw-2026-07-02.json
---

# Competitive research — workflow-engine authoring & DX

> Commissioned to ground the authoring-experience redesign in prior art (and prior
> **mistakes**) before writing more workflows. This is a **research annex** to the
> `aion-authoring` cluster [`DESIGN.md`](./DESIGN.md), not a competing design. The
> raw per-engine findings (24 systems) are in
> [`competitive-research-raw-2026-07-02.json`](./competitive-research-raw-2026-07-02.json).

## Headline: the research independently validates the existing cluster design

The outside-in competitive sweep converged on the **same thesis** the `aion-authoring`
cluster already states inside-out (ADR-014): *the typed Gleam module is the single
source of truth; every mirrored surface is generated; every observed surface is
projected; a visual canvas is a projection, never a second authoring source.* That
agreement is a strong signal we are aiming at the right target.

What the research **adds** on top of the existing cluster:
1. **Competitive grounding** — what the incumbents actually do and where they actually hurt (real dev pain, not marketing).
2. **A firm serialization verdict** for a no-derive-macro language, and the discovery of a **stated-vs-shipped contradiction**: ADR-014 says types-first, but the shipped `aion generate` is **schema-first** (`schemas/*.json` is the authored source and the Gleam types are generated *from* it). The shape is still described twice.
3. **Novel bets** Aion's substrate uniquely enables (below) that no incumbent has.
4. **A sequencing** that relieves textual authoring *without* delaying the imminent NOI-proof workflow.

## The landscape (comparison matrix)

| System | Authoring model | Serialization (auto vs hand / source of truth) | Determinism burden | Boilerplate | Visual editor | Agent support |
|---|---|---|---|---|---|---|
| **Temporal** | Code-first, fn = workflow; ctx + single input struct; separate activities; worker registers on task queue | **Auto** via DataConverter chain; SoT = native types. ~0/type. TS leaks Date/Map/Set; Py needs Pydantic | **Heavy** — no time/random/IO/threads; checked at replay; surfaces late | Modest per-wf but structural (wf+activity+worker+starter, 2 procs) | **None** first-party; Web UI = observability only | **Strong/recent** — OpenAI Agents SDK; `activity_as_tool` auto tool schemas; UI generic |
| **Restate** | Code-first HTTP-native; handler sig = typed I/O; `ctx.run("label",…)` marks durable boundary inline | **Auto** (serde/Jackson/JSON); SoT = native types; opt Zod→OpenAPI. ~0/type | **Medium** — non-det through `ctx.run`, author-visible | **Very low** — sig + order-of-ops + ctx.run labels; no worker file, no codec | **None** (deliberate); built-in journal UI | **Strong** — awakeables = HITL; wraps LangGraph/OpenAI |
| **DBOS** | **Durability-as-a-library** (in-process, Postgres); `@DBOS.workflow`/`@DBOS.step` on ordinary fns | **Auto** JSON; SoT = native types. ~0/type | **Low-medium** — non-det in steps | **Lowest textual** — 2 decorators + normal calls; no worker/broker/codec | **None** authoring; **Conductor** ops console: exec-graph, pause/resume/fork/restart-from-step | **First-class** — `DBOSAgent` one-line; Conductor real-time graph |
| **Inngest** | TS event-driven; `step.run("id", fn)` checkpoints; mounts in your HTTP app | **Auto** structural TS; SoT = TS types. 0 codec | **Low**; **#1 gotcha**: code outside `step.run` re-runs every invocation | **Very low** (~8 lines) | **None** authoring; strong Dev Server/inspector | **First-class AgentKit** |
| **Trigger.dev** | TS; `task({id, run})` long-running body, no step-wrapping; waitpoint tokens | **Auto via superjson** (Date/BigInt/Map survive); large payloads auto→S3 | **Almost none** — CRIU checkpoint/restore, no replay | **Low** (~6-8 lines) | **None** authoring; **Realtime** hooks for your own UIs | **Strong** — waitpoints = HITL, Realtime = live streaming |
| **Golem** | Any WASM lang; agent = durable stateful object; `this.value+=1` is durable | **Auto, type-derived** (serde / TS typegen→WIT); SoT = lang types. ~0 | **Almost none** — ALL non-det captured at WASI boundary & replayed | **Among lowest** (~6-10 lines all intent) | **None** — hosted Console only | **First-class agent runtime**; durable LLM record→replay; exactly-once tools |
| **Azure Durable Fns** | Code-first; orchestrator + activity + client; `await CallActivityAsync` = checkpoint | **Auto JSON reflection**; serialization-at-replay is a documented non-det source | **Heavy** — no Now/Guid/random/IO in orchestrator; JS no-async, Py generators | Moderate + big cognitive tax | **None** authoring; Durable Task Scheduler dashboard | **Rapidly first-class** (MS Agent Framework) |
| **AWS Step Functions** | **Declarative DSL** (Amazon States Language JSON/YAML) = deployed artifact | **Untyped JSON**; hand-threaded via JSONPath; no cross-state type check | **None** (managed); cost = 25k-event quota | Low-ceremony but wrong-altitude JSON | **Best round-trip** — Workflow Studio Design↔Code live bidirectional (ASL only) | **None** first-class; `.waitForTaskToken` = durable pause |
| **Camunda/Zeebe** | **Visual-first BPMN 2.0 XML** = executable + diagram; FEEL expressions | Model **untyped** (JSON vars, FEEL refs); worker typed via POJO. Described twice, no cross-check | **None** authoring; burden = worker idempotency (at-least-once) | Real logic hides in FEEL fields + worker + cluster ops | **Most mature visual** but **git-diff hell** (XML mixes logic+layout, 257-line diffs) | 8.8 AI connector; User Tasks = strong HITL |
| **Prefect** | Imperative Python; `@flow`/`@task`; dynamic DAG | **Auto/structural** — cloudpickle or JSON/Pydantic; SoT = hints. ~0 | **None** — no replay; retry = re-run | **Among lowest** (~6 lines) | Read-only run graph | Pydantic AI durable-execution |
| **Dagster** | **Declarative assets**; `@asset` fn, deps by param name | **Auto via IO managers** (pluggable); codec factored OUT, written once | **None** — re-materialize; version/staleness first-class | Low body, heavy framework scaffolding | Richest lineage UI (read-only) | Weakest (poll sensors) |
| **Airflow (TaskFlow)** | `@dag`/`@task`; deps inferred from passed returns | **Auto via XCom, JSON-ONLY**; custom types need serialize()/deserialize() | **None** replay; **top-level code trap** (re-parsed ~30s) | TaskFlow cut legacy ceremony | Read-only Graph/Grid | Weakest; batch |
| **LangGraph** | Py/JS; Graph API (State + add_node/edge) or Functional (`@entrypoint`/`@task`) | **Auto/structural** JsonPlus; SoT = Py types. Sharp edges (Pydantic→dict, msgpack) | Graph auto-persists at node boundaries; critics: **checkpoints ≠ durable** | Graph moderate; Functional near just-order-of-ops | **LangGraph Studio = best-in-class** — live state, **time-travel + interrupts + state editing** | **First-class**; `interrupt()` HITL; Studio = live obs + intervention (**the NOI bar**) |
| **Mastra** | TS; `createStep({inputSchema,outputSchema,execute})` + `.then().parallel().branch().commit()` | **Schema-first Standard Schema (Zod)** — ONE schema = type + validator + serialization. ~1/type | **Low-medium** — typed suspend/resume | Low; per-step schemas + `.commit()` | Playground: chat, hot model-swap, per-step traces (local) | **First-class**; suspend/resume HITL |
| **Inngest AgentKit** | TS; createAgent/createTool(Zod)/createNetwork | **Zod-first**; SoT = Zod→TS | **Strong durable** (step.ai journaled/retried/cached) | Low-moderate | **`useAgent`** React hook = parts-based real-time streaming, auto-resume (**closest analog to NOI**) | **First-class durable multi-agent** |
| **Protobuf/gRPC** | **Schema-first** `.proto` IDL; protoc/Buf generates types+codecs; field tags = permanent wire identity | **Zero hand codec**; SoT = schema; **no first-class Gleam/BEAM target** | Buf `buf breaking` = machine-checked compat; **field-tag stability = durable-history-safe** | Zero codec, pays IDL + tag ceremony | None | agent-neutral |
| **tRPC** | **Types-first, zero codegen**; client type INFERRED from `typeof appRouter` | Static: **zero** (pure inference). Runtime: hand Zod. **Inference EVAPORATES at runtime — no persistable schema** (fatal for durable) | Not durable | Lowest static; TS-only, monorepo-locked | None | The DX bar |
| **Encore.ts** | **Types-first + build-time analyzer EMITS schema**; plain TS interfaces; `api<Req,Res>()` | **Auto** — TS type IS contract; compiler derives schema; Rust core precompiles codec. **Zero codec, zero separate schema, AND an emitted durable artifact** | Emitted schema = versionable durable artifact | ~Zero serialization ceremony | Local dashboard (code-first viz) | None |
| **Effect Schema / Zod** | Types-first via **runtime schema VALUE**; one `Schema.Struct`/`z.object` → type + validator + enc + dec | ONE declaration, no drift; **needs no macro** (schema is a composable value); two-way transform | Schema is persistable/versionable; migrations = schema→schema fns | ~1 builder chain/type (not zero) | None | schema = LLM tool arg shapes |
| **Rust serde** | **Types-first via derive macro**; `#[derive(Serialize,Deserialize)]` at compile time | **Gold standard**: ONE attribute, zero codec, native speed. **Exactly what Gleam CANNOT do** | Deterministic; wire compat = author discipline (no Buf gate) | ~1 derive line | None | de-facto LLM tool codec in Rust |
| **Gleam JSON today** | **No derive/macros**: hand encoder + separate decoder that drift. Partial: codec/convert_json (1 bidir value), gserde/glerd (build codegen) | Baseline ~20 lines/type, shape described **3×** (Gleam type + JSON Schema + Rust struct) | Combinator = no drift; codegen = regen-drift risk; no Buf-style guard | Combinator ~⅓ hand; codegen = true zero | None | none |

## The ten universal pain points (ranked)

1. **Versioning / schema-drift of in-flight work** — the #1 wound across *every* replay/journal engine. Temporal `GetVersion` branch-bloat + late `NonDeterministicException`; DBOS "finishes on the version it started"; Azure "Non-Deterministic workflow detected"; LangGraph's "orphaned thread" schema-drift crisis; Step Functions 25k-event cap; Camunda manual instance migration. **Nobody has solved evolving code/types against durable in-flight history.** Only Trigger.dev's atomic pin and Buf's field-tag gate point at real answers. **The single biggest opportunity for Aion.**
2. **The determinism footgun** — replay engines force authors to police what code is legal (no time/random/IO/threads) and violations surface *late, at replay*. The two engines that ELIMINATE it — **Golem** (capture at the WASI boundary) and **Trigger.dev** (CRIU checkpoint/restore) — are the ones authors call friendliest. Determinism-transparency is a competitive axis, not a fixed cost.
3. **Serialization boilerplate is concentrated exactly where the host language lacks reflection/derive.** Every mainstream engine gets codecs ~free (Go/TS/Py/Rust). **Gleam is the outlier** — the whole industry already routed around this by making TYPES the source of truth and deriving everything. Aion's 3× description is a Gleam-specific tax, not a universal one.
4. **Visual workflow artifacts destroy git.** BPMN/n8n/Node-RED interleave logic + layout → one-attribute changes become 257-line diffs, unresolvable merge conflicts. **Windmill is the ONLY one that won** — plain files (flow.yaml + per-step code + separated lock) engineered so PRs are reviewable. Any Aion visual editor MUST keep a reviewable textual source.
5. **The workflow↔worker process split** (4 moving parts) is real ceremony the newest systems collapse (DBOS in-process, Inngest mounts-in-app, Restate co-locates). Increasingly seen as legacy friction.
6. **Large payloads / history bloat** force manual workarounds everywhere (S3 claim-check, XCom backends). Trigger.dev auto-uploads >512KB transparently. **Aion's content-addressed haematite store is uniquely positioned to make this invisible.**
7. **Local dev / testing friction** is chronic. Winners treat the inner loop as first-class (Temporal in-memory time-skipping env; Windmill `wmill dev` live-reload; Mastra Playground).
8. **Idempotency / saga compensation** stays the author's burden even in "durable" systems — durability = exactly-once *replay*, not exactly-once *external effect*.
9. **Untyped data between steps** in declarative/visual systems (Step Functions JSONPath, Camunda FEEL, n8n) → runtime failures, not author-time. **A typed engine that carries types THROUGH the boundary beats this.**
10. **Operational heaviness** gates adoption (Temporal needs DB+Elasticsearch+services; Camunda needs a K8s Zeebe cluster). The pull is toward "library, not infra."

## Best-in-class per axis

- **Economic textual authoring:** **DBOS** (2 decorators + ordinary calls; nearly every line is intent), Golem ties conceptually. *Why:* collapsed the wf/worker split AND lean on host reflection. **The target shape for Aion.**
- **Serialization (no-macro language):** **Encore.ts** — build-time static analysis of language types → EMITTED schema → precompiled codec. Serde economy *without* macros, *and* a durable versionable artifact. (serde is the gold standard but needs macros Gleam lacks; Effect Schema is best for the runtime-value approach and is achievable in Gleam today.)
- **Determinism transparency:** **Golem** (all non-det at the WASI boundary; ordinary imperative code is durable-safe), Trigger.dev second (CRIU).
- **Visual editing (round-trip):** **AWS Step Functions Workflow Studio** (genuine live bidirectional Design↔Code). **Camunda** wins visual maturity but loses to git-diff hell.
- **Code↔visual round-trip:** **Windmill** — decisively the most relevant prior art (plain files, `wmill dev` disk↔canvas, neither side a lossy projection).
- **Agent observability / intervention:** **LangGraph Studio** (live node execution + time-travel + interrupts + **state editing** persisted to history) — but dev-local, LangChain-only, snapshot-not-durable. **Inngest AgentKit `useAgent`** is the best durable-backend→live-frontend streaming. **Both are the bar Aion's NOI must beat, and both have the exact weakness Aion can exploit: not durable + not distributed.**

## Serialization verdict (for a Gleam engine, no derive macros)

**Types-as-source-of-truth via an Aion-owned BUILD-TIME codegen** (the Encore.ts pattern: parse the language's own types, EMIT codecs + schema) — **not** hand-written combinators, **not** a schema-first IDL, **not** structural inference.

- **Reject combinator values** (codec/convert_json, the Effect-Schema-in-Gleam model) as the *destination* — they collapse 3 declarations to 1 bidirectional value (huge win, no drift) but still cost a builder chain per type. **Keep as the manual escape hatch.**
- **Reject schema-first IDL** (protobuf/Smithy) as the primary surface — forces authors out of Gleam's type system into a second DSL (describes the shape twice), and there's no first-class Gleam/BEAM protoc target. **But steal Buf's field-tag stability + `buf breaking` gate for the emitted history-contract layer.**
- **Reject structural inference** (tRPC) — the inference evaporates at runtime; **no persistable schema to decode old durable history against. Fatal for a durable engine.**

**One source (the Gleam type) → four artifacts:** (a) Gleam boundary codecs, (b) a serde-compatible contract for the Rust/Python worker, (c) a durable-history contract with stable field-ids + a breaking-change gate, (d) — see novel bet #3 — the agent tool schema. This unifies the current 3× description AND attacks the #1 universal wound (versioning). **Regeneration-drift is the one real risk — mitigate by wiring codegen into the build so a stale codec fails compilation** (`aion generate --check` discipline already exists for the schema path).

## Aion proposal

### Thesis
> The durable-execution engine where **a workflow is just your types and your order of operations**, every in-flight run is a **live editable transcript backed by real crash-survivable history**, and the **same ONE type declaration** drives the Gleam codec, the Rust/Python worker contract, the agent tool schema, AND the durable-history compatibility gate.

Aion sits in an **unclaimed quadrant**: DBOS/Golem/Restate give near-zero-ceremony textual authoring but **no durable-distributed live-agent observability+intervention**; LangGraph Studio/AgentKit give live agent UX but **snapshot-not-durable, dev-local, single-language**. Aion is the only stack with (a) a typed workflow language, (b) polyglot durable workers, (c) a content-addressed durable store, AND (d) a durable + real-time observability/intervention plane (NOI). We match the incumbents' textual economy **and** own live durable agent intervention.

### Textual redesign — two taxes, two fixes
**1. Kill the run-ceremony (pure SDK + package change, no codegen).** Every workflow hand-writes the same ~27-line `run(raw_input: Dynamic)` decode/run/encode dance, yet `workflow.define(...)` already bundles name+input_codec+output_codec+error_codec+entry_fn. **Fix:** add `workflow.entrypoint(definition) -> fn(Dynamic) -> Result(String, String)` to the SDK; the engine `run/1` shim becomes one line. Zero change to the engine boundary contract. Highest-leverage, lowest-risk change.

**2. Kill/relieve the codec tax.** Finish migrating authoring onto generated codecs so no project hand-writes them, and resolve the source-of-truth tension (below) so the schema isn't a hand-written third artifact.

**Target minimal workflow (the whole module, after redesign):**
```gleam
import aion/workflow
import app/types.{type HelloInput, type Greeting}   // types + codecs generated

pub fn definition() {
  workflow.define("hello", types.hello_input_codec(), types.greeting_codec(), types.err_codec(), execute)
}
pub fn run(raw) { workflow.entrypoint(definition())(raw) }   // the only glue line

fn execute(in: HelloInput) -> Result(Greeting, WorkflowError) {
  use g <- result.try(workflow.run(greet(in)))   // one activity, typed
  Ok(g)
}
```
`execute` is pure intent; `run` is one line; every codec is generated. That is DBOS/Golem economy, reached in a no-macro language via Aion-owned codegen.

### Serialization recommendation (+ migration)
Make the **Gleam type the single declared shape**; Aion codegen emits the four artifacts above. Resolve the **stated-vs-shipped contradiction**: ADR-014 says types-first, but shipped `aion generate` makes `schemas/*.json` the source and generates types *from* it.
- **Step 1** — retarget codegen INPUT from `schemas/*.json` to the Gleam type declarations, emitting the same io/codecs/worker/goldens. Authors delete `schemas/`; the diff to generated outputs should be near-empty (that near-empty diff *is* the verification).
- **Step 2** — migrate order-fulfillment (the flagship, still on hand-codecs) onto the generated path.
- **Step 3** — add stable-field-ids + the breaking-change gate to the emitted history contract.
- Keep the combinator-value codec library underneath as the documented escape hatch.

### Novel bets (Aion's substrate uniquely enables)
1. **Pin-data from real durable history.** n8n's most-loved inner-loop feature (freeze a node's real output, iterate downstream without re-hitting APIs) — Aion does it *natively and better*: every activity result is already content-addressed in haematite. "Replay but pin `charge_payment` to the exact recorded result from run X" = a store lookup by content hash. No incumbent has real-durable pin-data because none has a content-addressed history.
2. **Time-travel + state-edit on distributed durable history**, not dev checkpoints. Same UX as LangGraph Studio, backed by crash-survivable cross-node history. NOI already locks "intervention is a durable observability record but NOT part of replay" — that exact separation is what makes state-edit safe against replay, which LangGraph gets wrong.
3. **One type declaration → agent tool schema, for free.** The boundary codegen already parses activity I/O types; make it ALSO emit OpenAI/Anthropic-compatible tool schemas for activities used as agent tools. One derivation → Gleam codec + worker contract + history gate + agent tool schema.
4. **Types carried through the boundary** beat every untyped visual engine — the canvas shows real typed wires, not stringly-typed JSONPath.
5. **Durable-history-safe evolution** via content-hash module namespacing (already present: `logical_name$hash` pins in-flight runs to their version) + protobuf-style stable field-ids + a machine-checked breaking-change gate. Old haematite history decodes against evolved types by construction, checked in CI. **The strongest evolution story in the category** — the #1 universal wound.

### Visual / DX roadmap (reuse the ops-console + NOI, don't build a new canvas)
- **Phase 0 — Live run visualiser (read-only)** over NOI: render a running workflow as a live graph (activities, timers, signals, child workflows, agent steps) driven by the NOI event stream + recorded history. Lowest-risk, highest-demo-value. DBOS-Conductor / Step-Functions-graph parity.
- **Phase 1 — Intervention + time-travel** on the live visualiser: wire NOI intervene/approval/cancel into the graph; time-travel scrubbing over durable history + content-addressed pin-data. **This is where Aion passes LangGraph Studio.**
- **Phase 2 — Semantic (not textual) diff + deploy view:** structural diff of workflow *shape* (topology/boundary-types), surface content-hash version pinning so operators see which in-flight runs are on which version.
- **Phase 3 — Bidirectional code↔visual for COARSE topology only** (the hard, later bet): the canvas owns coarse control flow and DROPS TO REAL GLEAM for any non-trivial expression. NEVER interleave layout with logic (derive layout from code, or sidecar). Gleam stays the source; the visual is a VIEW.

### Sequencing (protect the NOI proof)
- **CUT 1 (days, before the NOI-proof workflow):** add `workflow.entrypoint(definition)`; one-line `run` shims. Self-contained, no engine change. Write the NOI-proof workflow with `execute` + one-line `run` from day one.
- **CUT 2 (small-medium, parallel):** the NOI-proof workflow uses the existing schema-first `aion generate` codecs — **zero hand-written codecs** — even before the source-of-truth swap.
- **CUT 3 (medium, AFTER the proof ships):** retarget codegen to Gleam types; migrate order-fulfillment. The strategic "declare once" win.
- **CUT 4 (parallel once CUT 3 stabilises):** visual Phase 0/1 (live NOI run-visualiser + intervention/time-travel).
- **CUT 5 (later):** stable-field-ids + breaking-change gate; then bidirectional code↔visual.

**Rule:** CUTs 1+2 unblock the imminent proof and are cheap. Never let 3/4/5 delay shipping the NOI proof.

### Risks & open questions
1. **Gleam codegen feasibility:** the facts extractor (`crates/aion-package/src/structure/facts.rs`) is a *tokeniser*, not a type-checker. Deriving full boundary codecs from Gleam types needs real type resolution (nested types, generics, options, unions). Materially harder. Open: build a Gleam type reader, lean on the compiler/LSP, or keep schema-first as the pragmatic 90%. **The pure types-first ideal may be multi-quarter; schema-first-but-single may be the shippable 90%.**
2. **Regeneration-drift** — mitigate by wiring codegen into build/`aion check` (discipline already exists for the schema path).
3. **Determinism must be preserved** — the entrypoint adapter is pure decode/encode (safe); time-travel state-edit + pin-data touch history semantics — honor NOI's "durable-but-not-replay" rule exactly or recreate LangGraph's orphaned-thread bug.
4. **Scope creep vs shipping the NOI proof** — the sequencing (CUTs 1+2 only before the proof) is designed to prevent this.
5. **Source-of-truth decision must be OWNED** — resolve the ADR-014 (types-first) vs shipped (schema-first) contradiction, or new workflows keep landing on inconsistent paths (order-fulfillment hand-codecs vs order-saga generated).
6. **Visual layout/diff** — never persist coordinates in Gleam source (sidecar or derived) or re-import Camunda's git-diff hell.
7. **Polyglot contract skew** — one contract for Gleam + Rust serde + Python + agent tool schemas multiplies the surface that must stay byte-compatible with recorded history; the wire-compat goldens are the guard.

## Reconciliation with the existing `aion-authoring` cluster

- **Validated by the research** (keep as-is): ADR-014 (typed module = source of truth, no DSL); the declare-once codegen goal; `aion dev` instant loop; the oplog-is-the-debugger lens; the canvas-as-projection principle; the determinism static gate (P7 — directly answers universal pain #2).
- **Sharpened by the research** (a real gap to close): the cluster *states* types-first (ADR-014) but the *shipped* `aion generate` is schema-first — the WA-001…007 briefs should explicitly own the retarget (CUT 3) or amend ADR-014 to bless schema-first-as-single-source. Codegen today covers workflow-I/O only, not per-activity (the cluster already names this).
- **New from the research** (fold into the cluster): the five **novel bets** (esp. content-addressed pin-data, durable time-travel beating LangGraph Studio, one-type→agent-tool-schema, stable-field-id evolution gate); the **Windmill files-as-source-of-truth** lesson for the canvas (pain #4); the **sequencing** that protects the NOI proof.
