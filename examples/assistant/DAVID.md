# The assistant — hand-over crib sheet

An aion-authoring assistant that runs AS an aion workflow: a norn agent in a
durable, operator-paced chat session. You start it with an objective, it
answers from the real repo (docs, examples, the Gleam SDK), and between
rounds it waits for your next message. Ask it aion questions; have it author
new workflow packages; make it explain every build/deploy step it proposes.

Everything below runs from the aion repo root unless stated.

## 0. Prerequisites (one-time)

1. **norn installed and authed.** The worker strips `OPENAI_API_KEY` from
   norn's child environment, so norn's ChatGPT OAuth login is what
   authenticates. Prove it end to end:

   ```bash
   norn -p --fast "say ok"
   ```

   The adapter version-gates on `protocol: "norn-driven/1"` and refuses a
   stale binary. Verify (must print `"protocol":"norn-driven/1"`):

   ```bash
   printf '{"jsonrpc":"2.0","id":1,"method":"initialize"}\n' \
     | norn --protocol jsonrpc | head -1
   ```

2. Optional: `AION_WORKSPACE_ROOT` — where session workspaces are created.
   Unset = `~/.aion/clones` (fine).

## 1. Build the artifacts

```bash
# The workflow packages
cargo run -p aion-cli -- package examples/assistant --build   # -> examples/assistant/assistant.aion
cargo run -p aion-cli -- package examples/agent-dev --build   # -> examples/agent-dev/agent-dev.aion

# The ONE worker binary that serves both packages
cd examples/agent-dev/worker && cargo build && cd ../../..
```

(A separately managed assistant worker is a later phase; today the agent-dev
worker serves `assistant` and `assistant_provision` alongside the agent-dev
activities.)

## 2. Start the server

```bash
cargo run -p aion-cli -- server --config examples/agent-dev/demo-config.toml
```

Ops console at **http://127.0.0.1:8080**. The demo config has
`[deploy] enabled = true`, which the console deploy surface needs.

## 3. Deploy the packages (through the console)

Console → deploy → upload `examples/assistant/assistant.aion` (and
`examples/agent-dev/agent-dev.aion` if you want the dev pipeline too).
Expect `assistant` in the workflow/version list.
(Terminal alternative: `cargo run -p aion-cli -- deploy examples/assistant/assistant.aion`.)

## 4. Start the worker

```bash
./examples/agent-dev/worker/target/debug/agent-dev-worker --address 127.0.0.1:50061
# add --norn-bin /path/to/norn if norn on PATH is not the hardened build
```

Expect a startup line naming the resolved workspace root, then
`connected and registered; serving dispatches`.

## 5. Start an assistant session

**Console:** the Assistant panel (being built alongside this workflow) —
or start workflow → type `assistant` → input:

```json
{
  "objective": "Teach me how signals work in aion, then help me add one to a workflow.",
  "repo_path": "/Users/tom/Developer/ablative/aion"
}
```

- `repo_path` is the aion repo the assistant grounds itself in (local path
  or clone URL). Its workspace is a CLONE of it, so nothing it edits touches
  your checkout. Empty `""` = scratch workspace, no repo — it will tell you
  what it cannot verify.
- A blank `objective` is rejected at the input boundary.

**Terminal fallback:**

```bash
cargo run -p aion-cli -- start assistant \
  --input '{"objective":"How do I add a signal to my order workflow?","repo_path":"/Users/tom/Developer/ablative/aion"}'
```

or raw HTTP:

```bash
curl -sS -X POST http://127.0.0.1:8080/workflows/start \
  -H 'content-type: application/json' \
  -H 'x-aion-subject: david' -H 'x-aion-namespaces: default' \
  -d '{"namespace":"default","workflow_type":"assistant","input":{"objective":"...","repo_path":"/Users/tom/Developer/ablative/aion"}}'
```

Within seconds: `assistant_provision` records the workspace clone, then the
`assistant` round dispatches and the live transcript starts streaming.

## 6. Chatting — the two channels

There are two different ways your words reach the agent; use the right one:

- **While a round is RUNNING** (transcript streaming): use **intervene →
  inject** on the running attempt. The message lands at the agent's next
  tool boundary, mid-thought. Priority `interrupt` for "stop, change
  course"; normal for "also consider this".
- **Between rounds** (`assistant_status` = `awaiting_operator`): send the
  **`assistant_continue`** signal. Payload contract (ONE signal, the payload
  discriminates — see §9):
  - continue: `{"message": "your next message"}`
  - end the session cleanly: `{"end": true}`

  ```bash
  cargo run -p aion-cli -- signal <WORKFLOW_ID> assistant_continue \
    --payload '{"message":"Now show me the codegen step, please"}'
  cargo run -p aion-cli -- signal <WORKFLOW_ID> assistant_continue \
    --payload '{"end":true}'
  ```

  or raw HTTP:

  ```bash
  curl -sS -X POST http://127.0.0.1:8080/workflows/signal \
    -H 'content-type: application/json' \
    -H 'x-aion-subject: david' -H 'x-aion-namespaces: default' \
    -d '{"namespace":"default","workflow_id":"<WORKFLOW_ID>","signal_name":"assistant_continue","payload":{"message":"Now show me the codegen step"}}'
  ```

Forgiving by design: a blank message, `{}`, or an unparseable payload is a
no-op nudge — it never kills the session. `{"end":true}` wins over a message
riding in the same payload. The session is bounded at **50 agent rounds**;
hitting the cap completes the run as `round_cap_exhausted` (start a fresh
session — the workspace persists).

Live state any time: query `assistant_status` → `{phase, round}`
(`provisioning` / `working` / `awaiting_operator`):

```bash
cargo run -p aion-cli -- query <WORKFLOW_ID> assistant_status
```

## 7. One conversation across rounds — why it holds

The worker pins the norn session id to `{workflow_id}-{activity_type}` with
`--resume-if-exists`. Every round dispatches the SAME activity type
(`assistant`), so every round — and any retry after a worker restart —
resumes the ONE session `<workflow_id>-assistant`. The workflow therefore
sends continuation messages VERBATIM (no context repetition): the session
already holds the contract and the whole conversation.

The full conversation persists at `~/.norn/sessions/<workflow_id>-assistant.jsonl`;
the session's workspace at `~/.aion/clones/<workflow_id>/repo` (or your
`AION_WORKSPACE_ROOT`). Both survive the run for inspection.

**What the assistant can touch:** its file tools (read/write/edit) are
confined to the workspace clone (`--workspace-root`); its SHELL commands
start in the clone (`-C`) but can read elsewhere on the host — that is how
it can `cat` the original repo if it ever needs to. It works in a clone, so
your checkout is never modified; anything it authors, you copy out.

## 8. When it wedges

- **A round runs forever / went off the rails:** intervene → **cancel** on
  the attempt (graceful: the envelope comes back and the norn session
  persists). NEVER kill the worker to stop a step.
- **The run Failed or you cancelled the whole run:** Reopen from the
  console re-drives it; recorded rounds replay from history and the live
  round resumes its session.
- **Worker process died mid-step:** just restart it; the engine re-delivers
  and `--resume-if-exists` resumes the same conversation. But see sharp
  edges — don't kill it on purpose.

### Sharp edges (known, real)

- **No engine-level retry safety net beyond the workflow's own retry policy
  (#197):** don't kill the worker mid-step to "restart" anything; use
  cancel/reopen.
- **Graceful-restart parity (#207) is pending:** stopping the worker while a
  round is in flight is recoverable but not yet seamless — prefer letting
  the round finish or cancelling it first.
- The session waits indefinitely in `awaiting_operator` — that is by design
  (no timeouts on operator time). An abandoned session is ended with
  `{"end":true}` or cancelled from the console.

## 9. The signal contract (for tooling authors)

Verified against the deployed package and the server signal API
(`POST /workflows/signal`, `ProtoSignalRequest`):

- **workflow_type:** `assistant`
- **input:** `{"objective": string, "repo_path": string}` (both required;
  `repo_path: ""` = scratch mode)
- **control signal:** `assistant_continue` with payload
  `{"message"?: string, "end"?: boolean}` — `{"message": "..."}` continues,
  `{"end": true}` ends cleanly.
- There is deliberately NO separate `assistant_end` signal: the engine's
  selective receive parks on exactly ONE signal name at a time, so a second
  name could never wake a waiting session without timer-polling (which would
  grow durable history unboundedly while the session idles). One name,
  payload-discriminated, is the composition that works.
- **queries:** `assistant_status` → `{"phase": string, "round": int}`
- **output:** `{"disposition": "operator_ended"|"round_cap_exhausted",
  "rounds": int, "last_reply": string, "workspace_path": string}`

## 10. Reset between takes

Server data in `aion-data/` (delete for a fresh server), workspaces under
`~/.aion/clones/`, norn sessions under `~/.norn/sessions/`. Each session is
keyed by its own run id — old sessions never interfere with new ones.
