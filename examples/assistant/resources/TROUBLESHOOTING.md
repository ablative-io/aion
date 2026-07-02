# TROUBLESHOOTING.md — aion authoring lifecycle, symptom → cause → fix

Read this before telling an operator "that should work." Every command below
assumes you are at the aion repo root unless stated. CHECK, never assume:
whether `aion`, `gleam`, `git`, `norn` are on PATH; which binary version is
running; whether the server/worker you're diagnosing is even the one you
built. Run `which aion gleam git norn` and `aion --version` before reasoning
about a failure — a stale binary explains more incidents here than any code
bug.

## 1. `workflow type <X> is not registered`

**Where you'll see it:** server API error on `start`, exact server-side
message shape is `format!("workflow type {workflow_type} is not
registered", ...)` (`crates/aion-server/src/api/handlers/error.rs`).

**Cause:** the `.aion` package for that workflow type was never deployed to
the server you're talking to, or it was deployed to a different namespace
than the one you're starting in.

**Fix:**
1. Confirm the target server/namespace: `aion versions --namespace <ns>
   --endpoint <endpoint>` and look for the workflow type in the list.
2. If absent, deploy it: console → deploy → upload the `.aion` archive, or
   `aion deploy <path/to/pkg.aion> --namespace <ns>`.
3. If present under a different namespace, either redeploy into the right
   one or pass `--namespace` correctly on `start`. Namespace defaults to
   `default` on every CLI subcommand — an operator who typed `--namespace
   foo` on `deploy` but not on `start` (or vice versa) is the single most
   likely mistake here.
4. Never assume "I deployed it earlier" is still true — servers restart,
   `aion-data/` gets wiped between takes (see §10 reset below), and a fresh
   server has nothing loaded.

## 2. Started workflow sits `Running` / pending forever, nothing dispatches

**Cause, in order of likelihood:**
1. No worker process is connected that serves that workflow's activity
   types (worker never started, or crashed).
2. A worker IS connected but was built against an OLDER package version —
   its registered activity types don't match what the new package's
   `workflow.toml` declares (stale binary after you edited
   `<pkg>_activities.gleam` and regenerated).
3. Namespace/task-queue mismatch between what the workflow was started in
   and what the worker announced.

**Fix:**
1. Check the console's worker/cluster view (or `aion describe <id>` timeline)
   for a connected worker. A worker that never logged `connected and
   registered; serving dispatches` is not serving anything.
2. If a worker is connected but nothing moves, RESTART it after rebuilding:
   ```bash
   cd examples/<pkg>/worker && cargo build
   ./target/debug/<pkg>-worker --address 127.0.0.1:50061
   ```
   The engine re-delivers to the freshly-registered worker; you do not need
   to touch the workflow.
3. Confirm namespace agreement: whatever `--namespace` the workflow was
   started under must be one the worker was told to serve.
4. Do NOT assume a pending activity will eventually time out and retry
   itself into visibility — see §7, there is deliberately no step timeout,
   so "pending forever" really does mean forever until a worker shows up.

## 3. Workflow FAILED, `kind: Terminal`, message starts with `retryable:...`

**This looks like a contradiction — it isn't; read it literally.**

**Cause:** the engine has **no activity-level retry policy applied here**
(or the workflow author didn't attach one). An activity error whose message
is prefixed `retryable:` is a *signal from the activity author's code* about
the failure's nature (see `crates/aion/tests/order_saga_e2e.rs`,
`ActivityErrorKind`), but if nothing durable is watching for that prefix and
re-scheduling — e.g. the worker died mid-step, or no `RetryPolicy` was
attached to the activity call — the outcome that gets RECORDED is `Terminal`.
A worker process dying mid-activity-execution is itself terminal for that
attempt; there is no "engine-level retry safety net beyond the workflow's
own retry policy" (tracked as #197 — confirmed still open, do not tell an
operator this is fixed).

**Fix:**
- Do not kill the worker mid-step "to see what happens" — that is exactly
  how you manufacture this failure. If a step is wedged, use
  intervene → **cancel** on the attempt (graceful), never kill -9 the
  worker.
- If it already happened: `aion reopen <WORKFLOW_ID>` (only valid on
  `Failed` or `Cancelled` runs) re-drives it — recorded history replays,
  the failed step re-executes fresh.
- If you author activities that need real retry, attach a `RetryPolicy` at
  the call site in the workflow's Gleam source; the engine will not invent
  one for you.
- Verify which behavior you're actually looking at with `aion inspect
  <WORKFLOW_ID>` (time-travel over the event-store oplog) before telling
  the operator a diagnosis — don't guess from the top-level status alone.

## 4. `generate --check` fails / "codec drift"

**Exact failure modes seen in the codebase:**
- `--check failed: {activities module} activities list is out of date; run
  \`aion generate\`` — `workflow.toml`'s `[[workflow]].activities` list no
  longer matches the package's `manifest()` output.
- Any generated file (`<pkg>_codecs.gleam`, `<pkg>_activity_wrappers.gleam`,
  schemas under `schemas/*.json`, the wire-compat golden, the worker module)
  differs from a fresh in-memory regeneration.

**Cause:** someone hand-edited a generated file, or ran `gleam format` /
an editor auto-formatter over it. **Generated files are derived from
`src/<pkg>_io.gleam` (the authored source of truth, ADR-014) — they are not
meant to be edited OR reformatted by hand.** `aion generate --check`
compares byte-for-byte against a fresh regeneration; formatting drift is
still drift.

**Fix:**
```bash
aion generate <project_path>
```
This regenerates every derived artifact and writes them. Never hand-patch a
generated file to make `--check` pass — regenerate from the types module
instead. If `--check` still fails after regenerating, the authored
`<pkg>_io.gleam` or `<pkg>_activities.gleam` changed in a way that alters
derivation — that's expected; commit the new generated output.

**Also watch for:** a stale `.aiongen.lock` file at the project root
(`.aiongen.lock`, created via `create_new` for single-flight safety). If a
prior `aion generate` was killed (not exited cleanly) mid-run, this lock can
be left behind and a fresh run will fail fast rather than proceed. Check for
and remove it only if you're sure no other `generate` is actually running.
Similarly a leftover `*.aiongen-aside` file (renamed-aside source) or a
stray `aion_generate_probe.gleam` module indicates a crashed prior run;
`aion generate` self-heals stale asides/probes on its next run, but if the
build is failing for a mysterious reason, `find <project> -name
'*.aiongen-aside'` is worth a look before you assume a real compile error.

## 5. `gleam test` fails on determinism (or `aion check --deterministic` flags a call)

**Exact gate output:** `` `aion check` currently performs the determinism
gate only; pass `--deterministic` to run it `` (if you forgot the flag), or
on a real violation:
```
determinism check failed: N non-deterministic call(s) reachable from workflow code.
Workflow code must read time via `workflow.now()` and entropy via `workflow.random()`
so replay is exact.
[{"workflow":...,"function":...,"call":"erlang.system_time",...,"kind":"wall_clock","remedy":...}]
```

**Cause:** workflow code (anything reachable from the entry function the
workflow's `workflow.toml` declares) calls a wall-clock or entropy primitive
directly — `erlang.system_time`, random number generation, `Date.now`-style
calls, UUID generation not routed through the SDK, etc. — instead of going
through the SDK's recorded primitives. On replay, the engine re-runs the
workflow body from history; a direct clock/entropy read returns a DIFFERENT
value the second time and the replay diverges (this is invariant 2 in the
design: workflow code must be a pure function of recorded history).

**Fix:** replace the offending call with the SDK's deterministic substitute:
- wall-clock reads → `workflow.now()`
- randomness/UUIDs/anything nondeterministic → `workflow.random()`

Run `aion check --deterministic <project_path>` locally before packaging —
it is meant to be a CI gate (C28) and will catch this before it reaches a
running server where the failure mode is a silent replay desync, not a
clean error.

**Do not put this logic in activities either** without checking — determinism
only matters for the *workflow* function itself; activities run once per
attempt and are allowed to touch the wall clock/entropy freely. If the gate
flags a call and you believe it's actually inside an activity, verify with
`aion inspect`/read the call graph before arguing with the tool — the
analysis walks the real call graph from the declared entry function, so a
flag means it IS reachable from workflow code.

## 6. Package builds fine but `deploy` rejects it, or you're confused about which version is live

**Exact error:** `package integrity mismatch: expected version \`{expected}\`,
computed \`{computed}\`` (`crates/aion-package/src/error.rs`).

**Cause:** aion workflow versions are **content-addressed** — the version
identifier is a hash derived from the package's contents. This error means
the `.aion` archive's declared version doesn't match what the server (or the
packager) recomputes from the bytes — almost always because the archive was
hand-edited/corrupted after packaging, or you're deploying a `.aion` file
that was built by a different/older `aion package` than the one computing
the check.

**Fix:**
1. Rebuild the package fresh, don't reuse an old `.aion` archive after
   editing source: `aion package <project_path> --build`.
2. **Any change to the package — including a codec/schema regen with no
   "meaningful" source change — mints a NEW content-addressed version.**
   Redeploy after every change; there is no "update in place." Old versions
   stay loaded (and routable) until explicitly `unload`ed.
3. Confused about what's actually live? `aion versions --namespace <ns>`
   lists every loaded version with its routing flag. Use `aion route` to
   point a workflow type's routing at a specific already-loaded version
   (rollback/roll-forward) — don't assume the newest deploy is automatically
   the one new `start`s will resolve to; check the routing flag.
4. `aion unload` only works on a version that is non-routed and unpinned —
   if it refuses, something is still pointing at it; check `aion versions`
   for the routing flag before assuming the command is broken.

## 7. An agent-driven activity step hangs / runs a very long time

**This is by design, not a bug.** There is deliberately **no step timeout**
in these agent-worker examples — an agent step (norn session) is allowed to
run as long as it needs; the engine will not kill it for you, and the
workflow will not time it out either.

**What to actually do:**
- Watch the live transcript in the console. If it's genuinely stuck (no
  tool calls, no progress) or went off the rails, use **intervene → cancel**
  on the running attempt. This is graceful: the envelope still comes back
  and the underlying norn session persists (it is NOT destroyed).
- If you need it to change direction rather than stop, use **intervene →
  inject** (or the console's chat-style intervention input) instead of
  cancelling. Priority `interrupt` lands at the next tool boundary for
  "stop, change course"; normal priority queues for "also consider this."
- **Never kill the worker process to interrupt a step.** See §3 — a worker
  death mid-step is recorded as a terminal activity failure, not a
  cancellation, and there is no retry safety net under it.
- An `awaiting_operator` workflow (e.g. the `assistant` example between
  rounds) waiting indefinitely is ALSO by design — no timeout on operator
  think-time. Abandoning it cleanly is a real action (send the `_continue`
  signal with `{"end": true}`, or console cancel), not something that
  resolves itself.

## 8. Norn-specific failures

### `protocol mismatch: expected "norn-driven/1"`
**Cause:** the adapter version-gates on the norn wire protocol and refuses
a binary that doesn't speak `norn-driven/1`. This fires INSTANTLY (the
activity fails before doing any real work) — a stale/mismatched norn binary
on PATH or passed via `--norn-bin`.
**Fix:** verify before you ever start a run, don't assume:
```bash
printf '{"jsonrpc":"2.0","id":1,"method":"initialize"}\n' | norn --protocol jsonrpc | head -1
```
Must print `"protocol":"norn-driven/1"`. If it doesn't, rebuild/install the
correct norn (`cd ~/Developer/ablative/norn && cargo build --release`) and
either put it first on PATH or pass `--norn-bin <path>` to the worker
binary, then restart the worker — the failed activity attempt will retry
and pick up the fix (subject to §3's caveat: only if a retry policy exists).

### Not authed / auth errors from norn mid-run
**Cause:** norn authenticates via ChatGPT OAuth; the worker deliberately
STRIPS `OPENAI_API_KEY` from norn's child environment, so an API key in your
shell env will NOT save you here — only a live OAuth session works.
**Check first, don't assume it's logged in:**
```bash
norn -p --fast "say ok"
```
If that fails or prompts a login, refresh the OAuth session, then the
failed activity attempt retries.

### Context-limit death of a long session
**Cause:** a long-running norn session (many rounds, large transcripts) can
hit the model's context limit mid-round. Observed live (2026-07-03,
Phase-2 NOI dogfood): the run failed and — separately, a console bug, not a
data-loss bug — the console showed nothing inspectable, because the
attempts list was a LIVE-only enumeration, not derived from durable history.
**What's actually true:** the failure is recorded honestly as a workflow
failure; the transcript itself IS durable (persisted in the O-keyspace /
`~/.norn/sessions/<workflow_id>-<activity_type>.jsonl`) even though a
context-limit death is a real, final failure for that session — it is not
silently swallowed. To inspect it: read the `.jsonl` session file directly,
or `aion inspect <WORKFLOW_ID>` for the oplog, or `aion describe
<WORKFLOW_ID>` for the recorded event history. Don't trust "the console
shows nothing" as "there's no evidence" — check the durable sources before
concluding the run is unrecoverable. The workflow itself is
`Reopen`-able if it terminated in `Failed`.

### Worker process died mid-step (norn specifically)
**Fix:** just restart the worker. The engine re-delivers and the worker
resumes the SAME norn session (pinned to `{workflow_id}-{activity_type}`
via `--resume-if-exists`) rather than starting a fresh conversation — this
is one of the few cases where a worker restart is safe and expected, as
long as you didn't kill it to try to interrupt a live turn (that's §3's
terminal-failure trap instead — the distinction is whether the death
happened to you or because of you).

## 9. Quick diagnostic commands (run these before speculating)

```bash
aion versions --namespace <ns> --endpoint <endpoint>      # what's actually deployed + routed
aion describe <WORKFLOW_ID>                                # event history / current status
aion inspect <WORKFLOW_ID>                                 # time-travel oplog
aion query <WORKFLOW_ID> <query_name>                       # e.g. assistant_status -> {phase, round}
git log --oneline -5                                        # confirm which aion revision you're actually on
which aion gleam git norn && aion --version                 # confirm the binaries in play, don't assume
```

## 10. Reset between takes (when in doubt, start clean and re-verify)

- Server durable state: `aion-data/` (delete for a fully fresh server — you
  will need to redeploy every package after this).
- Session/run workspaces (clones the assistant/agent-dev agents work in):
  `~/.aion/clones/<run-id>/repo` (or `$AION_WORKSPACE_ROOT` if set — check
  it, don't assume unset).
- norn conversation transcripts: `~/.norn/sessions/<run-id>-<activity>.jsonl`.

Each run/session is keyed by its own id, so old and new never collide — if
you see cross-contamination between runs, you are almost certainly looking
at the wrong id, not a real bug. Verify the id before escalating.
