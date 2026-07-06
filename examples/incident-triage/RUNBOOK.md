# Incident-triage bridge — operator runbook

The proof: a **prospekt**-validated `incident` document drives an **Aion**
workflow as typed structured input, and the workflow returns a typed structured
result — where the `triage` step is executed by a **real Norn agent in driven
mode** (`--protocol jsonrpc`), observable and intervenable live from the ops
console. You drive this by hand; every command below is copy-pasteable.

The `triage` activity runs one of two ways, selected when you start the worker:

- **driven (default)** — a real Norn agent triages the incident. Its transcript
  streams live to the ops console and you can inject/cancel the running attempt.
  The model is constrained to the `TriageSummary` shape by the demo's
  `schemas/output.json` output schema, so it returns exactly
  `{incident_id, severity, headline, next_action}`.
- **plain (`--plain`)** — the old deterministic severity→action logic, no AI.
  Returns the SAME structured shape, so the workflow and result are identical.
  Use it to A/B the driven run or to demo without a Norn login.

You will:

1. build the Gleam workflow and package `incident-triage.aion`,
2. start a fresh Aion server (with the liminal worker listener) with the package preloaded,
3. start the Rust `triage` worker (driven by default),
4. mint + fill + ready-check an `incident` with prospekt,
5. start the workflow passing the incident document JSON as input, and
6. watch the live transcript, then read the structured triage summary back.

Terminal legend: **T1** server, **T2** worker, **T3** you (prospekt + CLI),
plus the **ops console** in a browser at http://127.0.0.1:8080.

## 0. Prerequisites (once)

- Gleam CLI with Erlang/OTP on `PATH` (`gleam --version` >= 1.17).
- Rust toolchain (`cargo`).
- The `aion` CLI, installed from this checkout so its wire version matches the
  worker SDK (verified together at **0.8.0**):

  ```sh
  cargo install --path crates/aion-cli --locked
  ```

- **A configured, working `norn`** (driven mode only). Model/provider/key config
  comes from YOUR norn setup — this demo hardcodes no secrets. The worker strips
  `OPENAI_API_KEY` from norn's child environment so it uses your ChatGPT OAuth
  login (the project does not use API keys). Prove norn runs before you start:

  ```sh
  norn -p --fast 'reply with the word ok'
  ```

  It must print `ok`. The adapter also version-gates on the driven-mode protocol;
  confirm your norn speaks it (must print `"protocol":"norn-driven/1"`):

  ```sh
  printf '{"jsonrpc":"2.0","id":1,"method":"initialize"}\n' \
    | norn --protocol jsonrpc | head -1
  ```

- The `prospekt` release binary at
  `/Users/tom/Developer/ablative/prospekt/target/release/prospekt`. If missing,
  build it: `cargo build --release` in that repo.

All `aion` commands run from the **aion repository root** unless noted.

## 1. Build the workflow and package (T3)

```sh
cd examples/incident-triage
gleam build
cd ../..
aion package examples/incident-triage
```

This writes `examples/incident-triage/incident-triage.aion`
(workflow type `incident_triage`, entry `run`, one activity `triage`). The Gleam
side is UNCHANGED by the driven upgrade — the worker returns the same
`TriageSummary` shape whichever path serves `triage`.

## 2. Start a fresh Aion server (T1)

Use `examples/incident-triage/demo-config.toml` — it is the repo's dev config
plus the `[outbox]` block the driven worker needs: the worker connects IN to the
server's **liminal listener** (server-push dispatch) on `127.0.0.1:50061`.
Preload the package on the command line so nothing needs editing:

```sh
aion server --config examples/incident-triage/demo-config.toml \
  --workflow-package examples/incident-triage/incident-triage.aion
```

Leave it running. You should see `loaded workflow package incident_triage`, the
startup banner `version 0.8.0 ... workflow_package_count 1`, a line reporting the
**agent harness composed at the binary root: norn**, and the **liminal worker
listener on 50061**. The ops console is now at **http://127.0.0.1:8080**.

## 3. Build and start the Rust triage worker (T2)

The worker is a standalone crate (not a workspace member); build and run it in
place. In DRIVEN mode (default) it advertises `triage` as an AGENT activity and
serves it by driving a real Norn agent over the liminal server-push transport.

```sh
cd examples/incident-triage/worker
cargo build --release
./target/release/incident-triage-worker --address 127.0.0.1:50061
# add --norn-bin /path/to/norn if the norn you want is not first on PATH
```

Leave it running. You should see `mode="driven (Norn)"` in the startup line and
then `connected and registered; serving triage` — and the worker appearing in
the console's worker/cluster view. A quiet worker after that is a connected
worker.

**Plain A/B:** to serve the deterministic logic instead, start it with `--plain`
(or `AION_TRIAGE_PLAIN=1`). Everything else in this runbook is identical; the
only difference is no Norn runs and no live transcript appears.

```sh
./target/release/incident-triage-worker --address 127.0.0.1:50061 --plain
```

## 4. Mint, fill, and ready-check an incident with prospekt (T3)

Set up a scratch prospekt workspace and copy the `debug-loop` model out of the
prospekt repo's `models-draft` branch (it is not on the default branch):

```sh
PK=/Users/tom/Developer/ablative/prospekt/target/release/prospekt
DEMO=/tmp/incident-triage-demo

rm -rf "$DEMO" && mkdir -p "$DEMO"
"$PK" --root "$DEMO/.prospekt" init
git -C /Users/tom/Developer/ablative/prospekt archive models-draft models/debug-loop \
  | tar -x -C "$DEMO/.prospekt"
"$PK" --root "$DEMO/.prospekt" validate debug-loop
```

`validate` prints `model is valid — no findings`.

Mint an incident document (prospekt injects `id`, `model`, `model_version`,
`state`, and the `forensics` slot; the payload fields start empty):

```sh
"$PK" --root "$DEMO/.prospekt" new debug-loop incident
```

It writes `INC-001` to
`$DEMO/.prospekt/models/debug-loop/documents/incident/INC-001.json`.

Now fill it. To drive by hand, open that file in your editor and write the
`title`, `severity` (`sev1`|`sev2`|`sev3`), `observed`, `expected`, and
`environment.binary` / `environment.invocation` fields. Or paste this known-good
fill to keep moving:

```sh
cat > "$DEMO/.prospekt/models/debug-loop/documents/incident/INC-001.json" <<'JSON'
{
  "environment": {
    "binary": "aion 0.5.0 (commit c41b35c)",
    "model": "claude-opus-4-8",
    "invocation": "aion start incident_triage --input @INC-001.json"
  },
  "expected": "The worker should adopt the shard and reach write quorum on failover.",
  "forensics": [],
  "id": "INC-001",
  "model": "debug-loop",
  "model_version": 1,
  "observed": "After kill -9 of the owner node, the adopted shard's WriteMembership never reached quorum (required 2, acknowledged 1).",
  "severity": "sev1",
  "state": "open",
  "title": "Cross-node kill-9 failover stalls below write quorum"
}
JSON
```

Ready-check — this is the gate that says the document is fit to hand to the
engine:

```sh
"$PK" --root "$DEMO/.prospekt" check debug-loop --ready
```

Expect `documents are ready — no findings` (exit 0). If a field is still empty
it lists exactly which one; fill it and re-run.

## 5. Start the workflow with the incident as typed input (T3)

Feed the ready document straight into `aion start` as the workflow input:

```sh
INPUT=$(cat "$DEMO/.prospekt/models/debug-loop/documents/incident/INC-001.json")
START=$(aion --subject incident-triage-user start incident_triage --input "$INPUT")
printf '%s\n' "$START"
WORKFLOW_ID=$(printf '%s' "$START" | python3 -c 'import sys,json;print(json.load(sys.stdin)["workflow_id"])')
printf 'workflow_id=%s\n' "$WORKFLOW_ID"
```

`start` prints `{"run_id":"...","workflow_id":"..."}`. The prospekt-injected
`model`, `model_version`, and `forensics` fields ride along in the payload and
are ignored by the workflow's decoder. The incident document JSON is exactly the
prompt the Norn agent receives; the triage instructions ride in the worker's
appended system prompt, and the output schema forces the response shape.

## 6. Watch the live transcript, then read the result (T3 + console)

Within a second or two of `start`, the run reaches `Running` and the `triage`
attempt dispatches to the worker, which starts a Norn agent in driven mode.

**In the ops console (http://127.0.0.1:8080):** open the run. The **live
transcript** for the `triage` attempt streams as the agent works — assistant
messages, the `structured_output` tool call, token/usage progress, and a
terminal stop. Because the attempt is a live driven session, the console's
**intervene** controls act on THIS attempt: an `inject` steers the running agent
(queued or interrupt priority) and `cancel` ends it gracefully (the envelope
still comes back; a re-dispatch `--resume-if-exists` resumes the same session).

**From the CLI**, wait for completion and read the structured result:

```sh
aion --subject incident-triage-user describe "$WORKFLOW_ID" --pretty
```

The `summary.status` is `Completed`, and the `WorkflowCompleted` event carries
the structured triage summary the Norn agent produced (same
`{incident_id, severity, headline, next_action}` shape the plain path returns).

## Verified output (driven mode)

A live driven round trip returns the agent's structured triage. Because a real
model writes `next_action`, its wording varies run to run (the plain path's map
is fixed); `incident_id`, `severity`, and `headline` are grounded in the
document. Example `WorkflowCompleted` result:

```json
{
  "incident_id": "INC-001",
  "severity": "sev1",
  "headline": "[sev1] Cross-node kill-9 failover stalls below write quorum",
  "next_action": "Page the on-call distributed-systems owner to inspect the adopted shard's WriteMembership/adoption path and restore write quorum."
}
```

For the **plain** path (`--plain`), the severity→next-action map is fixed:
`sev1` → page on-call, `sev2` → assign an owner and fix within the working day,
`sev3` → backlog for next sprint (anything else → clarify severity). Fill a
different `severity` in step 4 to see it change.

## What-ifs (driven mode)

| Symptom | Meaning | Do |
|---|---|---|
| worker log: `protocol mismatch: expected "norn-driven/1"` | stale norn binary | install a norn that speaks driven mode (§0), restart the worker; the retry re-dispatches |
| worker log: auth/provider errors from norn | ChatGPT OAuth session lapsed | refresh your norn login; the failed attempt retries |
| activity fails: `run stopped without completing` | the model hit a stop other than `completed` (timeout, schema-unreachable, cancelled) | the failure text carries the stop reason + detail; re-run, or serve `--plain` to A/B |
| no transcript in the console | worker started `--plain`, or not connected to the liminal listener | confirm the worker log says `mode="driven (Norn)"` and `connected and registered`; confirm the server logged the liminal listener on 50061 |

## Clean up

Stop the worker and server with `Ctrl-C` in T2 and T1, then:

```sh
rm -rf /tmp/incident-triage-demo \
  examples/incident-triage/incident-triage.aion \
  examples/incident-triage/build \
  examples/incident-triage/worker/target \
  aion-data
```
