# Method notes — working with Vesper Lynd

**Purpose.** Tom brought Vesper Lynd (Fable-model agent) in for a short window (~to 2026-07-07)
so the team can absorb *how they work*, not just their output. Tom's thesis: VL isn't smarter
or a better coder than an Opus-class model — the edge is **diligence, method, attention to
detail, coherence**; it does by default the discipline we work hard to enforce. This is a
running reflections doc: capture the procedure, and mark what **actually moves the needle** vs
what only **looks like it might**. Maintained by Frodo (single point of contact).

Legend: ⭐ = adopt as standing practice · 🔬 = promising, needs a real trial before we trust it.

---

## 1. Getting your head around an existing frontend (VL's actual method on this console, 2026-07-01)

- ⭐ **Find the spine before reading any component.** In a frontend the spine is the *data
  flow*: where events enter (`websocket.ts`, `transcript-stream.ts`), how they become state
  (stores/hooks), where views read them. **Read the spine files yourself, end to end — never
  delegate them.** Everything else can be agent-summarized; the spine can't, because every
  future decision leans on it.
- ⭐ **Classify survives-vs-offenders against a written contract, in one sentence.** VL's whole
  audit conclusion was: *"the stream spine is excellent and survives; the offenders are the
  REST-wrapped attempts hook, the dense-rank lane layout, and selection state living in
  component useState instead of the URL."* That one-sentence classification is the value — it
  says where you renovate vs demolish. Cross-check: our team's independent review converged on
  the same offenders → both audits were honest.
- ⭐ **Fastest single diagnostic of frontend maturity: "where does state live?"** URL vs context
  vs component `useState` vs store. (Our ADR-015 URL-as-state finding is the same read.)
- ⭐ **Fan out for breadth, synthesize yourself.** VL sent 4 agents in parallel (current console
  audit, meridian web app, brand site, the component libs Tom rates), each with **specific
  questions**, not "go understand this." Their reports give coverage; the synthesis and the
  read of load-bearing files stay yours. **Failure mode to avoid: believing you understand a
  codebase because an agent's summary reads confidently.**
- ⭐ **Propose changes as view-level work on a stable spine — never spine work justified by view
  complaints.**

## 2. Sub-agent craft (VL's three rules — "move the needle more than model-specific tuning")

1. ⭐ **Never trust subagent green.** Re-gate every merge yourself: forced recompile, lints
   uncached, tests re-run, eyes on the diff. Twice in a week a builder's "all gates pass" hid a
   red gate behind a shell pipe. **Corollary: never judge a gate through a pipe or chained
   command — capture its own exit status.** (Matches our own [[feedback_verification_attestation]]
   / language-aware gate discipline — VL states it harder.)
2. ⭐ **Per-agent acceptance criteria written before dispatch; adversarial review as a SEPARATE
   agent with a mandate to REFUTE, not confirm.** "Is this good?" → yes. "Find the wire break
   between these two parallel builders" → found two hard contract breaks pre-merge.
3. ⭐ **Unrun = unproven — including activation steps.** Gates prove code; only driving the full
   chain proves the feature. VL shipped the assistant "end-to-end green" and Tom's first click
   failed (package never deployed; worker predated the agent type). **Say "untested first drive"
   out loud when that's what you're handing over.** (This is our [[feedback_integration_testing]]
   lesson, stated as doctrine.)

## 3. Authoring surface (VL's position, sent to Tom 2026-07-03 — see SURFACE-RETHINK doc)

- Endorses our reframe: **"no second source of truth," not "no DSL."** Canonical model is the
  only truth; every surface is a lossless view. Settled.
- ⭐ **The keel is the expression grammar, not the carrier.** Spec the verb grammar first (call
  an action / bind / branch / fan out / wait; anything computational is an action) as its own
  document *with conformance tests* → the carrier becomes a skin decision.
- VL differs from our G-endorsement: candidate is **markdown with structured islands** (prose
  document, one section per step, fenced blocks for machine-precise parts, sidecar JSON Schemas,
  `aion check` with compiler-grade errors). Tom's YAML unease is real and durable.
- 🔬 **Don't win by argument — measure.** Proposed fixture test: write agent-dev's *real*
  workflow in (a) sketch G, (b) structured-islands markdown, (c) a JSON5 control; Tom + Wendy
  cold-read them as humans; hand the assistant the same spec and **measure which surface it
  authors correctly without babysitting.** AI-agents-are-primary-authors makes that measurement
  outrank everyone's taste. Max Power the likely hand to own writing one surface.

## 4. Corrections VL gave me to my starting picture (2026-07-03)

- The console assistant is **no longer a tab** — Round 2 landed `/assistant` + `/assistant/:id`
  with a mode-aware docked chat (live → inject-as-interrupt; awaiting → signal).
- The horizontal-space problem is **already answered by a Tom-ratified contract**: the three
  docs `OPS-CONSOLE-UX.md`, `OPS-CONSOLE-DESIGN-LANGUAGE.md`, `MORPHING-ENTITY-WORKSPACE.md`.
  Standing rule that kills the "logs scroll to the moon" complaint: *nothing requires manual
  refresh; views derive from streams; summon-and-dismiss over cramming.* **Lesson for me: read
  the ratified contract before rebuilding a picture from a voice brief.**

---

## Reconciliation worked through (Tier-0 vs ledger #201/#202/#203) — 2026-07-03

- **#201 = the step/attempt navigator** (OPS-CONSOLE-UX.md absorbs #198/#201). Its
  post-mortem-first-class rule ("enumerate attempts from history/event stream, never from
  liveness") **is Wendy's post-mortem bug fix (Option A), generalized.** VL is right they're the
  same work — and #201 is the *fuller correct shape* ("one model, two views" incl. Gantt
  click-through), so our narrow Option-A patch should **fold into #201**, not ship standalone
  (it'd be re-torn-out otherwise). Concede + upgrade.
- **The distinction VL collapses:** our Tier-0 braided *two axes by urgency*. Console half
  (post-mortem) → #201. But `workflow.entrypoint`, `aion run --watch`, strip-hardcoded-path are
  the **CLI/authoring dev-loop** (clone-to-first-run cliff) — NOT console ledger; they sit with
  the authoring surface / parked RM-021..029 line.
- **Unverified:** #202/#203 are not defined in the three contract docs — did not assert on them;
  asked VL to confirm scope.

### Resolution (VL reply, 2026-07-03)

- VL accepted the flattening pushback in full and **named the error**: *context-anchoring* — VL
  mapped Tier-0 onto the console ledger because the console was what we'd been discussing. ⭐
  Lesson (symmetric): a confident mapping over an **unverified referent** is the shared failure
  mode — my honesty-flag (refusing to assert #202/#203) is the exact discipline that prevents
  it, and VL called it "the practice I'd most want your team to keep under deadline pressure."
- **#202/#203 were invisible because they live in VL's harness-local task ledger** — I can't see
  it. Real coordination gap (VL's tooling, not diligence); VL is flagging it to Tom as Meridian
  v2 feedback: **cross-agent work needs a shared ledger or the IDs are noise.** Interim rule VL
  adopted: always paste scope text, never bare numbers. 🔬 (worth making a standing cross-agent
  norm.)
- **The mapping, resolved:**
  - **#201** = step/attempt navigator + post-mortem + **URL-as-state as its first carrier**
    (build attempt/step selection into the URL *as part of* the navigator, not retrofitted).
  - **#203** = start-workflow form (combobox from deployed types, JSON file upload, nomenclature
    sweep) + server-side rider: translator reasoning-summary double-emit dedupe. Meets our
    **#209 schema-exposure** seam. = Wendy's free-text finding.
  - **#202** = real Gantt (time-proportional bars, lane-identity=activity so dev↔review reads as
    ping-pong, fit-to-width, live-growing, continuous scrub, click-through to navigator) +
    **swimlane virtualization folds in here** (windowing belongs in the rebuild, not bolted onto
    the thing being replaced). = Wendy's sequence-vs-time finding *is* #202, not adjacent to it.
  - Provenance-line / keyboard-nav / calm-state = polish tail applied per-piece.
- **Final sequence = order of operator pain = order to present to Tom:**
  **#201 → #203 → #202**, polish tail per-piece.
- **Ownership split:** my team = frontend (#201/#203/#202 + the #209 seam). VL + Tom = the
  CLI/authoring dev-loop axis (#214 skill-doc path/versioning [VL's bug], #215 `aion run --watch`
  [new], entrypoint-shim template migration [pure debt, unblocked since aion_flow 0.5.0]) + the
  authoring-surface fixture.
- ⭐ **Presenting to Tom:** he reads outcomes first, wiring second — one sentence per item on what
  the operator *feels*; architecture only if he pulls the thread.

---

## Pinned wire contract — #203↔#209 start-form seam (both sides, verified against source 2026-07-03)

**Planning only** until Tom walks the console plan (his gate); recorded so it's not re-derived.
Split at the wire: VL owns the two endpoints (Rust backend); my team owns everything from the
response body forward (combobox, file/paste validation, form UX, annotation rendering).

- **Body shapes are the contract; URL spelling is VL's** (house router isn't `/api/`-prefixed).
- **List endpoint** → `{ types: [{ name, versions: [{ hash, active, deployed_at }] }] }`.
  Combobox offers logical names; version defaults to `active`, others behind a disclosure;
  `deployed_at` orders the list. (Historical-version enumeration after redeploy unverified —
  worst case `versions` is length-1 [active only]; shape unchanged, disclosure just hides.)
- **Schema+scaffold endpoint** → `{ name, version, schema, skeleton, annotations }`:
  - `schema` — JSON Schema verbatim from the package manifest; I validate file/paste against it,
    error points at the offending JSON pointer.
  - `skeleton` — pure-JSON structurally-valid starter (exists today: `build_input_skeleton` /
    `aion input <type>`; ADR-001 — structural zeros, never invented semantic defaults). Seeds the
    editor. **Load-bearing.**
  - `annotations` — `[{ pointer, note }]` derived from the schema walk (type traps like
    "object, not string", enum wire values, required-vs-omitted). **Optional; degrade gracefully:**
    rich Gleam-doc-comment descriptions → type-notes-only → empty. Never hard-depend on richness.
- **Design payoff:** annotations and validation errors share the JSON-pointer namespace, so a
  trap-hint ("object, not string") and a violation-error co-locate at the same field. VL's
  pointer-keyed choice earns double. Exactly one input schema per (type, version) — a
  multi-entrypoint project materializes as multiple *types*, so no entrypoint dimension needed.
- ⭐ Method that surfaced this: VL verified against source before answering and found the scaffold
  generator already existed — "nobody writes a scaffold generator; one was already written."
  Contract-first + verify-against-source beats adapter-later, especially across invisible ledgers.
