# Changelog

All aion crates share one workspace version; entries below cover the
whole stack (crates.io) plus the `aion_flow` Gleam SDK (hex) where noted.

## 0.9.2 — 2026-07-20

Out-of-box authoring, the central Aion home, security hardening of the
default-on studio surface, and honest event-stream recovery semantics.
Every change below survived independent adversarial review to a
zero-findings verdict (6 safety passes on the authoring/home branch,
5 correctness passes on the stream branch).

- **Aion home centralizes server configuration and state.** `AION_HOME` wins,
  otherwise the server uses `~/.aion`, without creating it during config reads.
  Server config discovery is now explicit `--config` > project-local
  `./aion.toml` > `<AION_HOME>/config.toml` > built-in defaults; every discovered
  file remains a loud typed failure when unreadable or invalid. Unconfigured
  haematite data and AWL studio roots are now `<AION_HOME>/data` and
  `<AION_HOME>/authoring`. If the old CWD-relative `aion-data` or
  `aion-authoring` directory exists, the corresponding unconfigured default
  keeps using it and emits an unambiguous startup migration warning naming both
  paths; explicitly configured paths never activate the guard. Sensitive roots
  are private by contract: on Unix Aion creates directories as `0700` and files
  as `0600` independent of umask, and an existing permissive Aion home fails
  startup with `chmod 700` remediation. Setting `AION_HOME` explicitly does not
  opt into a shared-home mode; shared Aion homes are unsupported. On non-Unix
  targets Aion does not claim to install or validate an owner-only ACL: startup
  fails closed for default home, data, authoring, and authoring-state roots.
  Operators must pre-provision private directories and explicitly configure
  `AION_HOME`, `store.data_dir`, and `authoring.workspace_dir`; accepted explicit
  roots emit a loud startup warning that ACL privacy was not verified.
- **AWL studio works out of the box.** A stock `aion server` exposes the full
  document, layout, check-backed editing, direct deploy, revision, run-status,
  and scaffold experience under the Aion-home authoring root, creating document
  and state directories on first write. Operators can still set
  `authoring.workspace_dir` / `AION_AUTHORING_WORKSPACE_DIR` explicitly. CN7 is
  unchanged: the separate Gleam loop stays unmounted until `gleam_path` is set.
- **The studio surface authenticates.** With `[auth]` enabled, every AWL
  document, layout, check, revision, run-status, and availability route
  requires a valid caller; document/layout/run-binding mutations require the
  deploy grant, and `worker_availability` is namespace-scoped. Auth-off stock
  behavior is unchanged. Workspace filesystem access is confined through held
  no-follow directory capabilities (symlinked legacy roots are never adopted;
  temp files are randomized `create_new`), and AWL `schema()` imports are
  confined to the workspace.
- **Path-ambient data-root protection (macOS).** Where the haematite backend's
  post-startup I/O is path-based rather than descriptor-authoritative, startup
  now requires every ancestor of the resolved data root to be owner-controlled
  — POSIX modes and Darwin extended ACLs both checked, ACE principals compared
  by UUID identity from a single owned ACL snapshot. New helper crate
  `aion-darwin-acl` carries the minimal Security-framework binding.
- **Honest event-stream contract and recovery.** The ops console speaks the
  server's real per-workflow `resume_from_seq` cursor (replay proven by
  integration test), runs one server-enforced subscription per socket, and
  never claims recovery it cannot prove: durable feeds recover via cursor
  replay with cursor-advance only after successful application; live-only
  feeds (filtered/firehose/cluster) degrade visibly with an explicit
  possible-gap state cleared only by a confirmed, quiesced, generation-guarded
  refetch. Cluster frames are contract-validated and each fresh snapshot is a
  new sequence epoch, so a server restart can no longer silently mute deltas.
  `WorkflowTimedOut` remains exported but unwired (no runtime emission
  exists); its build-or-drop decision is recorded as an open ruling.

## 0.9.1 — 2026-07-19

Dependency-alignment release — no aion surface changes.

- **haematite 0.4.1 → 0.5.0** and the **liminal family to 0.3.0**
  (`liminal-rs` 0.3.0, `liminal-sdk` 0.3.0, `liminal-server` 0.3.0) in one
  motion: `liminal-rs` 0.3.0's error types speak haematite 0.5.0, so the
  two pins are coupled and move together. haematite 0.5.0's kind-aware
  merge surface (its deliberate break) touches no API aion consumes.
- The embedded liminal worker front door passes `websocket: None` and
  `participant: None` for `liminal-server` 0.3.0's two new optional
  sections — documented byte-identical to the pre-0.3.0 build. Electing
  either is a future feature decision.

## 0.9.0 — 2026-07-18

Workspace version **0.9.0**. beamr pin now **0.15.4**. Three new crates
published for the first time this release: `aion-awl`, `aion-awl-lsp`,
`aion-awl-package`. ~660 commits since 0.8.0 (2026-06-29); 0.7.0 and 0.8.0
shipped without changelog entries — see the stub below.

### Language & authoring

- **AWL, a new first-party workflow language.** Full toolchain landed in
  four waves (AWL-1..AWL-4 — lexer, lossless canonical-printer parser,
  typechecker, Gleam-source emitter), then a rev-2 front-end rebuild with
  three Tom-ratified language rulings. Flow vocabulary landed in six waves:
  B1 ergonomics (`const`, raw strings, JSON literals, `schema of`), B2 flow
  shape (`subflow`, `distribute`/`sequence`, `collect`, `visits`), B3
  canvas projection (scoped substep graphs in the console), B4 lowering
  (distribute/sequence fan-out, subflows, settled collection), B5 true
  parallelism (multi-step `distribute` lowers to implicit child
  workflows), B6 direct-path distribute parity. `aion awl check` / `fmt` /
  `emit` give a typecheck-gated authoring loop from the CLI.
- **Types-first codec generation; schema-first removed.** The authored
  source of truth is now the Gleam types module `src/<package>_io.gleam`
  (types only); `aion generate` derives the codecs module
  (`src/<package>_codecs.gleam`), the EMITTED `schemas/*.json` artifacts,
  and the activity plumbing. **`aion codegen` is removed** — migrate by
  stripping the generated header/functions from the io module, deleting
  authored schemas, and running `aion generate .` (recipe in
  docs/guides/codegen.md). The test scaffold is now write-once.
  `examples/order-saga` is migrated and drift-gated in CI.
- **Direct-to-bytecode compilation** (BC-0..BC-3, new `aion-awl-package`
  crate): AWL documents compile straight to BEAM bytecode — MIR lowering,
  select/regalloc/assemble, and a programmatic package assembler producing
  the `.beam` container and an embedded SDK closure with zero `gleam`
  invocation on the product path. Typed codecs pin codec identity.
- **One-motion `aion deploy` / `aion run FILE.awl`** (B-3): a `.awl` file
  goes from source to a running workflow in one command over the
  direct-compile path, no intermediate Gleam project. Studio deploy uses
  the same path — deploy identity is derived from the AWL declaration
  itself, never the uploaded file name or scaffold template (fixes a
  cross-type clobber, pins regression #21); timeout edits now correctly
  redeploy as a new version.
- **AWL LSP** (new `aion-awl-lsp` crate): a thin server over the one real
  checker giving semantic hover and go-to-definition. Editor integrations:
  a Zed extension (Rust/Wasm, grammar-pinned) and a zero-hard-dependency
  Neovim plugin, both backed by a public `tree-sitter-awl` grammar.

### Runtime & engine

- **Engine auto-retry (#197).** Retryable activity failures are retried
  automatically at the dispatch seam, driven by a policy that was already
  on the SDK wire but unplumbed. `aion_flow` gains the matching config.
- **In-VM activity execution tier (Cut 3).** `InVm` activities now execute
  for real via a new engine NIF, `aion_flow_ffi:dispatch_activity_in_vm/4`,
  spawning the SDK-composed runner thunk as a LINKED child process of the
  workflow process (beamr 0.12.0's `Scheduler::spawn_link_closure`).
  Recorded-result semantics match remote activities by construction; a
  runner crash surfaces as a terminal `ActivityFailed` and the workflow
  process survives. New worker-free example: `examples/invm-demo`.
- **Remediation early-abort.** Identical-failure early-abort plus wave
  non-cascade on child failure, so one bad branch doesn't take the whole
  remediation wave down with it.
- **Durable timers and reopen.** Reopened runs get their durable timers
  rearmed again (#222/#223, previously lost on reopen); Failed/Cancelled
  workflows can be reopened end-to-end from the console (#180); a
  workflow-level durable Pause/Resume (#204) landed with a dispatch-hold
  enforced on the backpressure claim path.
- **Control-Plane, Phase 1 + 2.** A durable namespace registry
  (`NamespaceRecord`/`NamespaceStore`) minted on first `START`, backed by a
  quorum-CAS haematite implementation (`aion-store-haematite`); a
  namespace placement API with `Prefer` dispatch spill; per-namespace
  keyed backpressure at the outbox claim. Surfaced on the ops console.
- **kill-9 parked-timer failover.** A `#[ignore]`d repro (#148) pins the
  known post-kill-9 adopted-shard quorum-membership bug so it can't regress
  silently while it's worked; CI keystone adds a path-filtered kill-9 lane.

### Server & console

- **Ops console** (renamed from "dashboard", #156): always embedded by
  default (#154, the `embed-dashboard` feature is gone), running on the
  real wire contract (ts-rs-exported DTOs field-for-field), with
  browser-safe WS auth and config-driven CORS. A full design-system pass
  (one token vocabulary, terracotta/strategy-blue accent swap, motion kit,
  central keybinding registry, omni-palette); the **authoring studio**
  (CM6 editor, worker-backed tree-sitter highlighting, idle diagnostics,
  deploys-green strip, fmt-as-delta, flow-vocab canvas projection);
  **time-swimlanes** (time axis, true sub-lanes, continuous scrubber) and
  **nested-swimlanes** (recursive embedded child-run views with durable
  backfill); a durable attempt navigator, an assistant panel (session
  list/chat, `sendSignal`), a zustand draft-state layer, and a
  scrubber-continuity + Tab-indent UX pass.
- **NOI, a harness-agnostic observability layer.** A durable transcript
  spine (dedicated keyspace, single-writer sequencer) with mid-run
  intervention routing (operator → server → worker), a console
  `TranscriptPanel` with capability-gated `InterventionControls`, and a new
  `aion-integrations` `AgentHarness` SDK crate with two independent
  integrations (the norn adapter and `aion-integration-cli`). Durable
  attempt identity is now carried on activity lifecycle events (NOI-0).

### Workers & integrations

- **norn integration.** The norn adapter aligned to the `norn-driven/1`
  contract (stop envelope, protocol gate, tool cwd); a norn worker crate
  with developer and review-lens roles and a configured gate battery.
- **dev-brief, the general norn dev pipeline (#235).** A flagship
  AWL-authored example: developer runs the gate battery, concurrent
  adversarial review lenses judge the result, bounded fix cycles loop back,
  then land — with in-repo worktrees, reviewer read-only rooting,
  `{base_commit}` pinning, and a post-accept verify stage.
  (`dev_brief.awl`, `review_round.awl`.)
- **General-purpose worker crate**, a standalone worker honoring exact
  activity contracts without any language-specific SDK.
- `aion_flow` (hex) moved 0.5.0 → 0.6.0 → 0.7.0 across the range
  (`workflow.entrypoint` generated run adapter, AWL glue hoisted into the
  SDK, embedded closures regenerated against the current SDK).

### Dependencies

- **beamr 0.11.0 → 0.15.4.** Notable stops: 0.12.0 `spawn_link_closure`
  (the in-VM tier's foundation); 0.14.0 `loader/encode` (the `.beam`
  container writer behind the new `encode` feature, consumed by BC-3);
  0.15.2 routes `make_fun`/`put_map` near-full-heap allocation through the
  GC's `ensure_space` safety net and fixes an off-by-one DOWN-message heap
  reservation — closed a fatal `heap full` crash hit by aion's first
  direct-BEAM AWL child workflow in production; 0.15.3 adds a
  non-blocking, exactly-once exit-observation API (`take_exit_outcome`) and
  an uncompressed, byte-symmetric `.beam` `LitT` chunk (emitted bytes for
  any module carrying literals differ from 0.15.2's — expected, shifts
  package version identity once on next recompile); 0.15.4 adds an
  additive `BifRegistry::replace_existing` API.
- **liminal, promoted from an optional path-dependency spike (#13, feature-
  gated at 0.8.0) to the default workspace transport** — the ablative
  stack (haematite + liminal) is now the default build, libSQL/memory
  opt-in (#142/#143) — pinned through published 0.2.1 → 0.2.5 (`liminal-rs`
  0.2.5, `liminal-sdk` 0.2.4, `liminal-server` 0.2.5). 0.2.4's park-flip
  retires the embedded front door's per-connection busy-spin: idle
  connections now park on socket readiness instead of burning a slice
  every scheduler tick (measured ~140%+ CPU with zero connected workers
  before the fix); a push's reply deadline is now a pure wait quantum, no
  longer cancelled by an elapsed caller poll (G7).
- **haematite, promoted from a path dependency to a published crate**
  (crates.io, dropping the cross-repo path), pinned at 0.4.1 and now
  backing the durable namespace registry via `aion-store-haematite`'s
  quorum-CAS `NamespaceStore`.

## 0.7.0 / 0.8.0 — 2026-06

Both versions shipped without changelog entries. 0.7.0 (2026-06-22) bumped
beamr to 0.9.0 (cross-process local send + ref round-trip). 0.8.0
(2026-06-29) spiked and landed dispatch over liminal (#13, honest-ack
retry + dedup composition), started the haematite storage-swap design, and
rewrote the dashboard into a real-time ops console on the real wire
contract (browser WS auth, config-driven CORS). Their content is folded
into the history above; the AWL toolchain and everything else in this
document postdates both.

## 0.6.1 — 2026-06-13

### Engine (via beamr 0.6.1)

- **Workflow-process heap-reservation fix.** Pinned beamr 0.6.1, whose
  interpreter now runs `ensure_space` before the `put_list`/`put_tuple2`
  allocations, so a data-dependent burst of cons/tuple construction — e.g.
  decoding a large stage report inside workflow code — triggers GC and
  heap growth instead of dying with a fatal `heap full` that bypassed the
  collector. Surfaced by the brief_dev real-norn dogfood: a 12 KB scout
  report decoded in the workflow process crashed the run silently right
  after scout while a 10 KB one survived — a heap-reservation cliff, not
  genuine exhaustion (the process heap grows to ~1 MB). No aion code
  change; the bump adopts the VM fix. `aion_flow` is unchanged at 0.4.0.

### Notes

- Workflow code should stay thin (ADR-012): large activity results are
  best threaded as opaque payloads and decoded only by the consuming
  activity on the worker, where the heap is full-size and the work runs
  once rather than on every replay. The thin-workflow reshape is tracked
  as RM-023 and the standard library that makes it cheap as RM-022;
  observability so a crash never again looks like a hang as RM-024.

## 0.6.0 — 2026-06-13

### Engine

- **No default activity timeouts.** The engine-imposed 30s activity
  dispatch timeout is gone. Activity waits are unbounded and terminate
  only on completion, worker loss, server shutdown, or a workflow-level
  timeout the author chose. Agentic activities that run for an hour-plus
  are first-class.
- **Worker loss now delivers.** Losing the worker mid-activity fails the
  dispatch with a typed error instead of leaving the workflow parked
  forever (stream teardown previously only deregistered the worker).

### CLI

- **`aion new`** — scaffold a workflow project from four embedded
  templates: `hello_world`, `saga`, `approval_flow`, and `dev_pipeline`
  (the stacked-dev agentic pipeline, `--worker rust`). Scaffolds build,
  package, and pass their own test suites out of the box.
- **`aion codegen <dir>`** (and `--check`) — generate Gleam types and
  JSON codecs from JSON Schemas, with loud typed errors carrying file
  and RFC 6901 pointer context.
- Stale command hints in CLI output corrected for the unified `aion`
  binary.

### SDKs

- `aion_flow` **0.4.0** (hex) — `testing.mock_child`: typed child
  workflow doubles for unit-testing parent workflows without running
  their children. Scaffold templates now pin
  `aion_flow >= 0.4.0 and < 0.5.0`.
- `aion-rs` testing harness gains the matching in-process child doubles.
- Worker SDK logs session establishment; reconnect behavior hardened
  against server restarts.

### Examples

- `examples/stacked-dev` — the full agentic dev pipeline (provision →
  agent dev rounds → scoped verify → workspace gate → human review
  signal → land) with a standalone Rust activity worker, proven live
  against the real yg/norn/cargo/meridian CLIs end to end.
- Nested-workflow e2e suite: three-level chains, recursion,
  recovery-at-depth, and cancellation semantics pinned.

## 0.5.0 — 2026-06-11

- Unified `aion` binary (server runs as `aion server`; `aion-server` is
  lib-only).
- First release validated outside-in end to end (deploy → start →
  signal → query → recover).
