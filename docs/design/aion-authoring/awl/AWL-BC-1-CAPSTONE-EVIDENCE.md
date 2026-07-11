# AWL BC-1 capstone — evidence report (D-BC5)

Status: **PASSED** — both deliverables green, 2026-07-11.

Ratified criterion (`AWL-BC-DESIGN-DRAFT.md` §6 BC-1 row; `AWL-BC-BUILD-PLAN.md`
D-BC5): a hand-built minimal module loads, validates, calls `aion_flow`, and
completes a real engine run with an event trail matching its Gleam-built twin;
BC-2/BC-3 do not start until this is green. Per D-BC5 this ran in a path-dep
aion worktree and is an **evidence gate, not a merge** — the capstone harness
carries forward unmerged and becomes the seed of BC-4's differential harness.

## Where and what

- aion worktree: `.yggdrasil-worktrees/awl-bc1-capstone`, branch
  `awl-bc1-capstone` (commits `bf4bf969` patch setup, `e3169f37` harness).
- beamr encode worktree: `.yggdrasil-worktrees/awl-bc1-encode` at `76ce507`
  (`loader/encode/` behind the default-off `encode` feature).
- Worktree-only setup (never merges, by design): root `Cargo.toml` gains
  `[patch.crates-io] beamr = { path = ".../awl-bc1-encode/crates/beamr" }`
  and `encode` added to the workspace beamr features.
- Harness: `crates/aion/tests/awl_bc1_capstone.rs`; trail normalizer (BC-4
  seed): `crates/aion/tests/common/trail_norm.rs`; Gleam twin fixture:
  `crates/aion/tests/fixtures/capstone_twin/`.

## Exact commands (all run bare from the worktree root, exit codes shown)

```
cargo test -p aion-rs --test awl_bc1_capstone                  # exit 0 — 2 passed, 0 failed
cargo clippy -p aion-rs --test awl_bc1_capstone                # exit 0 (workspace lints deny warnings)
cargo fmt && git status --porcelain                            # clean apart from intended files
```

The test gate rebuilds both Gleam projects from committed source on every run
(`gleam build` via `common/example_build.rs` — no skip path), so the evidence
is reproducible from a clean tree with gleam 1.17 + erlc on PATH.

## Deliverable A — re-encoded module through the FULL production path

What ran (`deliverable_a_reencoded_awl_hello_matches_original_trail`):

1. `examples/awl-hello` built from committed source and packaged via
   `package_project` (the production packaging path).
2. **All 44 modules** of the package — the generated `awl_hello` entry module
   AND its complete SDK closure (`aion_flow`, `gleam_stdlib`, `gleam_json`) —
   decoded with `load_beam_chunks`, re-encoded with `encode_module`, re-decoded,
   and asserted structurally identical (`ParsedModule == ParsedModule`). No
   exclusions, no fallbacks: the BC-1 writer re-produced every module the
   loader could read.
3. The re-encoded bytes were rebuilt into a `.aion` archive through
   `PackageBuilder` and re-loaded through `Package::load_from_bytes`
   (integrity hash re-verified by the loader itself).
4. Both packages ran end-to-end through a real `Engine` (catalog load →
   `register_module_with_renames` → entry dispatch → live activity dispatch
   via a local deterministic dispatcher for `greet`/`shout` → durable
   recording in an `EventStore`).
5. Trails read back via `EventStore::read_history` and compared after
   normalization.

Results:

| | original | re-encoded |
|---|---|---|
| modules | 44 | 44 |
| content hash | `cd8b8ecc…47cbbf7a` | `fb9a2297…7752be55` |
| deployed entry | `awl_hello$cd8b8ecc…` | `awl_hello$fb9a2297…` |
| workflow result | `{"outcome":"shouted","payload":{"text":"HELLO, CAPSTONE!!!"}}` | identical bytes |
| durable trail | 8 events (Started, 2×[Scheduled/Started/Completed], Completed) | **identical after normalization** |

The differing content hashes mean the two runs exercised the content-hash
rename machinery with genuinely distinct rename maps over the same logical
modules — the named risk in the plan. The rename pass (module-name rewrite,
import rewrite, constant-pool rematerialisation, lambda unique_id recompute)
handled writer-produced bytes for all 44 modules without any observable
difference from erlc-produced bytes.

## Deliverable B — hand-CONSTRUCTED module

**Scope achieved: the FULL ratified criterion, not the fallback subset.**

`hand_built_capstone_module()` constructs a `ParsedModule` in Rust,
instruction by instruction — no Gleam source, no erlc, no bytes copied from
any compiled artifact anywhere in its production. The module: 8 atoms, 2
imports (`aion@duration:milliseconds/1`, `aion@workflow:sleep/1`), 1 export
(`run/1`), 2 tuple literals, 17 instructions (label/func_info/label, bare
`allocate`, `move` of the integer 25, two `call_ext`s into the SDK,
`is_tagged_tuple` on the `Result`, and a literal `move`/`deallocate`/`return`
per branch). Encoded size: **308 bytes** (the twin's erlc production of the
same behaviour: 2,216 bytes).

What ran (`deliverable_b_hand_built_module_matches_gleam_twin_trail`):

1. Explicit standalone `load_beam_chunks` → `resolve_imports` →
   `validate_module` over the encoded bytes: **accepted**.
2. Packaged with the twin's untouched SDK `.beam` closure through
   `PackageBuilder`, loaded through the catalog (rename machinery again, its
   own fresh hash), and run through the real engine.
3. The Gleam twin (`tests/fixtures/capstone_twin`, committed source, built
   from scratch by the gate) ran identically in a separate engine.

Results: both runs completed with result `"capstone"`; both durable trails
are exactly `WorkflowStarted → TimerStarted → TimerFired → WorkflowCompleted`
(the timer is `aion_flow`'s durable sleep — a real engine-side timer arm +
fire, not a stub) and are **identical after normalization**, including the
deterministic anonymous timer id. The normalized trail:

```json
["WorkflowStarted  {input: \"input is ignored\", run_id: <run-0>, package_version: <package-version>}",
 "TimerStarted     {timer_id: {Anonymous: 0}, fire_at: <time>}",
 "TimerFired       {timer_id: {Anonymous: 0}}",
 "WorkflowCompleted{result: \"capstone\"}"]
```

(Condensed; the full JSON is printed by the test under `--nocapture`.)

Honest inventory of what the hand-built module does NOT contain: activities,
codecs, closures (`make_fun`), `definition/0`, `module_info/0,1`. It is the
*minimal* workflow module of the ratified criterion — one exported entry
function calling `aion_flow`'s durable-timer surface — not a full
activity-workflow. The ratified criterion asked exactly for the minimal
module; the full shape inventory is BC-2/BC-3's job.

## Normalizer (BC-4 seed)

`common/trail_norm.rs`: serializes each `Event` to JSON and replaces exactly
the identity fields — `recorded_at`, `fire_at`, `workflow_id`, `run_id` /
`parent_run_id` (placeholders assigned in first-appearance order, so
multi-run trails compare positionally), and `package_version`. Everything
else must be byte-identical.

**Plan amendment note (BC-4 row):** the plan says "identical durable event
trails after run-id/`recorded_at` normalization". In practice the normalizer
necessarily also covers `workflow_id`, timer `fire_at` (derived from
wall-clock), and `package_version` — two byte-different productions of the
same module **always** hash differently, so `WorkflowStarted.package_version`
can never match raw. BC-4's harness must inherit exactly this field set; any
further field that needs normalizing should be treated as a divergence to
adjudicate, not silently added.

## Observations and surprises for the amendment clause

1. **No amendment-forcing surprises.** No validator rejection, no ABI
   mismatch, no missing opcode anywhere in the corpus the capstone touched.
   BC-2 may proceed on the plan as written.
2. **The writer's bytes are never erlc's bytes** (asserted per module): the
   canonical chunk set (`AtU8, Code, ImpT, ExpT, FunT, LitT, StrT, Line`)
   drops erlc's `Dbgi`/`Docs`/`Meta`/`Type`/`LocT` etc. Byte-differing but
   structurally identical is the designed contract — the round-trip equality
   is on `ParsedModule`, and the engine behaviour is the proof it suffices.
3. **No `int_code_end` terminator**: beamr's decoder stops at opcode 3 *or*
   end-of-bytes, and the writer emits no terminator. Round-trips within beamr
   are unaffected (proven), but writer-produced `.beam` files would be
   rejected by OTP's loader. Fine for BC (beamr is the only consumer); worth
   remembering if BC-5 ever advertises the artifacts as OTP-loadable.
4. **`module_info/0,1` are not required** by beamr's load/validate/execute or
   by the catalog path — the hand-built module omits them entirely. erlc
   always emits them; BC-3's emitter can skip them (or add them trivially if
   OTP-parity ever matters).
5. **Empty `Line`/`StrT`/`FunT` chunks are fine end-to-end** — the loader
   treats absent optional chunks as empty, engine included.
6. **Rename machinery**: literal atoms inside `LitT` tuples (`ok`, `error`)
   are correctly left alone by `rewrite_literal_atom` (only module-name atoms
   in the rename map are rewritten); `is_tagged_tuple` and `call_ext` survive
   `prepare_module` + rename + constant-pool rematerialisation untouched.
   Import rewriting to `<logical>$<hash>` worked for hand-built import tables
   exactly as for erlc ones.
7. **Anonymous timer ids are sequence-derived** (`{Anonymous: 0}`) and
   compare equal across backends without normalization — good news for BC-4:
   ids derived from deterministic sequence positions (activity ids too) stay
   comparable raw.
8. **The engine entry ABI observed directly** (useful for BC-2's MIR/ABI
   table): `run/1` receives the raw input payload as a term and must return
   `{ok, ResultBinary}` — the binary's bytes are recorded verbatim as the
   `WorkflowCompleted` result payload (here `"capstone"`, a JSON string,
   quotes included).

## Reproduction

From `/Users/tom/Developer/ablative/aion/.yggdrasil-worktrees/awl-bc1-capstone`
(requires the beamr encode worktree present at the patch path, gleam 1.17,
erlc):

```
cargo test -p aion-rs --test awl_bc1_capstone -- --nocapture
```
