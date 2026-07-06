# Incident-triage bridge — operator runbook

The proof: a **prospekt**-validated `incident` document drives an **Aion**
workflow as typed structured input, and the workflow returns a typed structured
result. You drive this by hand; every command below is copy-pasteable with no
placeholders to fill in.

You will:

1. build the Gleam workflow and package `incident-triage.aion`,
2. start a fresh Aion server with the package preloaded,
3. start the Rust `triage` worker,
4. mint + fill + ready-check an `incident` with prospekt,
5. start the workflow passing the incident document JSON as input, and
6. watch it complete and read the structured triage summary back.

Terminal legend: **T1** server, **T2** worker, **T3** you (prospekt + CLI).

## Prerequisites (once)

- Gleam CLI with Erlang/OTP on `PATH` (`gleam --version` >= 1.17).
- Rust toolchain (`cargo`).
- The `aion` CLI, installed from this checkout so its wire version matches the
  worker SDK (verified together at **0.8.0**):

  ```sh
  cargo install --path crates/aion-cli --locked
  ```

- The `prospekt` release binary already built at
  `/Users/tom/Developer/ablative/prospekt/target/release/prospekt`. If it is
  missing, build it: `cargo build --release` in that repo.

All `aion` commands run from the **aion repository root** unless noted.

## 1. Build the workflow and package (T3)

```sh
cd examples/incident-triage
gleam build
cd ../..
aion package examples/incident-triage
```

This writes `examples/incident-triage/incident-triage.aion`
(workflow type `incident_triage`, entry `run`, one activity `triage`).

## 2. Start a fresh Aion server (T1)

The repo-root `dev-config.toml` listens on gRPC `127.0.0.1:50051`, HTTP
`127.0.0.1:8080`, uses the `default` namespace, and (in this checkout) the
haematite store. Preload the package on the command line so nothing needs
editing:

```sh
aion server --config dev-config.toml \
  --workflow-package examples/incident-triage/incident-triage.aion
```

Leave it running. You should see `loaded workflow package incident_triage` and
the startup banner `version 0.8.0 ... workflow_package_count 1`.

## 3. Build and start the Rust triage worker (T2)

The worker is a standalone crate (not a workspace member); build and run it in
place. It registers the `triage` activity on task queue `default` and serves it
over the default gRPC poll transport.

```sh
cd examples/incident-triage/worker
cargo build --release
./target/release/incident-triage-worker
```

Leave it running. You should see
`worker session established; serving activities ... activity_types ["triage"]`.

Config is env-overridable if you need it (defaults shown):
`AION_WORKER_ENDPOINT=http://127.0.0.1:50051`, `AION_TASK_QUEUE=default`,
`AION_WORKER_IDENTITY=incident-triage-worker`, `AION_WORKER_CONCURRENCY=4`.

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
are ignored by the workflow's decoder.

## 6. Watch it complete and read the result (T3)

```sh
aion --subject incident-triage-user describe "$WORKFLOW_ID" --pretty
```

The `summary.status` is `Completed`, and the `WorkflowCompleted` event carries
the structured triage summary.

## Verified output (recorded 2026-07-06, aion 0.8.0)

A full live round trip was run end-to-end against this package and worker. The
final events of `describe --pretty`:

```json
    {
      "data": {
        "activity_id": 0,
        "result": {
          "headline": "[sev1] Cross-node kill-9 failover stalls below write quorum",
          "incident_id": "INC-001",
          "next_action": "page on-call and open a war room now",
          "severity": "sev1"
        }
      },
      "type": "ActivityCompleted"
    },
    {
      "data": {
        "result": {
          "headline": "[sev1] Cross-node kill-9 failover stalls below write quorum",
          "incident_id": "INC-001",
          "next_action": "page on-call and open a war room now",
          "severity": "sev1"
        }
      },
      "type": "WorkflowCompleted"
    }
```

`summary`:

```json
  "summary": {
    "status": "Completed",
    "workflow_type": "incident_triage",
    "started_at": "2026-07-06T02:30:05.072096Z",
    "ended_at": "2026-07-06T02:30:05.168874Z"
  }
```

The severity→next-action map is `sev1` → page on-call, `sev2` → assign an owner
and fix within the working day, `sev3` → backlog for next sprint (anything else
→ clarify severity). Fill a different `severity` in step 4 to see it change.

## Clean up

Stop the worker and server with `Ctrl-C` in T2 and T1, then:

```sh
rm -rf /tmp/incident-triage-demo \
  examples/incident-triage/incident-triage.aion \
  examples/incident-triage/build \
  examples/incident-triage/worker/target
```
