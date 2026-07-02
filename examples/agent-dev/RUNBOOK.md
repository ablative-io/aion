# agent-dev live-drive runbook

The NOI dogfood proof, driven by hand through the ops console: deploy the
`agent_dev` workflow, start the CHIRON-RUFF-001 brief, watch a real norn agent
work in the live transcript, redirect it mid-run with an intervention, watch
the cargo gate arbitrate, and inspect the landed diff.

Every command runs from the aion repo root unless stated. Expected output is
noted per beat — if what you see diverges, jump to the what-if table (§9) and
paste the named evidence back.

## 0. Prerequisites (one-time)

1. **aion main includes the norn-driven/1 alignment merge** (the adapter's
   envelope handling + version gate + worker `-C` arg). `git log --oneline -5`
   should show the `norn-align` merge.
2. **The hardened norn binary.** The adapter now version-gates on
   `protocol: "norn-driven/1"` and will refuse a stale binary by design.
   Install from the norn repo (main, ≥ the hardening merge):

   ```bash
   cd ~/Developer/ablative/norn && cargo build --release
   # then either install it as your `norn`, or note the path for --norn-bin
   ```

   Verify (must print `"protocol":"norn-driven/1"`):

   ```bash
   printf '{"jsonrpc":"2.0","id":1,"method":"initialize"}\n' \
     | norn --protocol jsonrpc | head -1
   ```

3. **ChatGPT OAuth login is live** (the worker strips `OPENAI_API_KEY` from
   norn's child environment, so the OAuth session is what authenticates).
   A quick `norn -p --fast "say ok"` proves it end to end.
4. Optional: `AION_WORKSPACE_ROOT` — where run workspaces are cloned.
   Unset = `~/.aion/clones` (fine).

## 1. Build the three artifacts

```bash
# The workflow package -> examples/agent-dev/agent-dev.aion
cargo run -p aion-cli -- package examples/agent-dev --build

# The worker binary
cd examples/agent-dev/worker && cargo build && cd ../../..
```

## 2. Start the server

```bash
cargo run -p aion-cli -- server --config examples/agent-dev/demo-config.toml
```

Expect in the boot log: the haematite store opening under `aion-data/`, the
worker heartbeat sweeper starting, gRPC on `50051`, HTTP on `8080`, and the
liminal worker listener on `50061`. The ops console is now at
**http://127.0.0.1:8080**.

## 3. Deploy the package (through the console)

Console → deploy → upload `examples/agent-dev/agent-dev.aion`.
Expect: `agent_dev` appears in the workflow/version list.
(Terminal alternative: `cargo run -p aion-cli -- deploy examples/agent-dev/agent-dev.aion`.)

## 4. Start the worker

```bash
./examples/agent-dev/worker/target/debug/agent-dev-worker \
    --address 127.0.0.1:50061
# add --norn-bin ~/Developer/ablative/norn/target/release/norn if norn on
# PATH is not the hardened build
```

Expect: a startup line naming the resolved workspace root, then
`connected and registered; serving` — and the worker appearing in the
console's worker/cluster view. A quiet worker after that is a connected
worker.

## 5. Start the run (through the console)

Console → start workflow → type `agent_dev` → paste the contents of
`examples/agent-dev/inputs/CHIRON-RUFF-001.json` as the input.
(Terminal alternative:
`cargo run -p aion-cli -- start agent_dev --input-file examples/agent-dev/inputs/CHIRON-RUFF-001.json`.)

Expect within seconds: the run appears Running; `provision` records (clone at
`~/.aion/clones/<run-id>/repo`, branch `agent-dev-CHIRON-RUFF-001`); then the
`scout` activity dispatches and **the live transcript starts streaming**.

## 6. The beats, in order

| Beat | What you see | Your move |
|---|---|---|
| scout | Transcript: norn reading the chiron layout, producing a plan | Watch. This is your warm-up; nothing to do. |
| dev starts | Transcript: reading `languages/typescript/biome.rs`, scaffolding a **compiled** adapter (likely `languages/python/`) | Wait for clear evidence it has committed to the compiled style (created/edited a file). |
| **THE INJECTION** | — | Intervene on the running `dev` attempt, priority **interrupt**: *"Change of plan: build the ruff adapter as a DECLARATIVE TOML adapter (see src/adapter/declarative/), not a compiled adapter. Remove any compiled-adapter scaffolding you already added."* |
| the redirect | Transcript: injection lands at the next tool boundary; the agent acknowledges, deletes/abandons the compiled scaffolding, pivots to the declarative engine | This is the money shot. |
| review | Fresh transcript judging against the acceptance criteria, ending in a JSON verdict | Fails → dev feedback round (same session, resumed — it remembers). Watch the loop. |
| gate | `gate` activity records pass/fail with diagnostics (cargo clippy + test inside the clone) | A fail feeds diagnostics back to dev — a legitimate demo beat, not a problem. |
| land | `land` records a commit sha; run completes `Passed` | Note the output fields: disposition, rounds, branch, workspace_path. |

Live narration any time: query `agent_dev_status` on the run → `{phase, round}`.

## 7. The closing beat

```bash
cd ~/.aion/clones/<run-id>/repo   # workspace_path from the run output
git log --oneline -3 && git show --stat
```

The diff is a declarative TOML ruff adapter — because you said so mid-run.
The full agent conversations also persist at
`~/.norn/sessions/<run-id>-{scout,dev,review}.jsonl`.

## 8. Timeouts and stuck runs

There is deliberately **no step timeout** — agent steps run as long as they
need. If a step ever wedges, use intervene → **cancel** on the attempt (the
graceful exit: the envelope still comes back and the session persists);
never kill the worker to stop a step.

## 9. What-ifs

| Symptom | Meaning | Do |
|---|---|---|
| dev/scout fails instantly: `protocol mismatch: expected "norn-driven/1"` | Stale norn binary | §0.2 — install the hardened norn, restart the worker (with `--norn-bin` if needed), the retry re-dispatches |
| Worker log: auth/provider errors from norn | OAuth session lapsed | `norn` login refresh; the failed attempt retries |
| Activity fails: `run resolved to a local slash command` | Prompt tripped norn's slash handling | Paste the failing prompt + transcript tail back to me |
| Review round burns on `unparseable verdict` re-ask | Model didn't end with the JSON object | One re-ask is built in; if it recurs, paste the review transcript tail |
| Run ends `ReviewCapExhausted`/`GateCapExhausted` | Honest budget exhaustion, not an error | Workspace + sessions persist; inspect, then Reopen from the console or restart with higher caps |
| Worker process dies mid-step | — | Restart it; the engine re-dispatches and `--resume-if-exists` resumes the same norn session rather than starting over |
| Anything else | — | Paste: worker stderr tail, the run's timeline screenshot/states, and the last ~20 transcript events |

## 10. Reset between takes

Server data lives in `aion-data/` (delete it for a fully fresh server),
workspaces under `~/.aion/clones/`, norn sessions under `~/.norn/sessions/`.
Each new run gets a fresh workspace keyed by its own run id — old takes never
interfere with new ones.
