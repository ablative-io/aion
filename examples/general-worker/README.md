# General-purpose worker (`GPW-1`)

This package is a standalone Rust worker for small workflows that need one driven Norn agent, arbitrary process execution, and deterministic text extraction. It serves exactly three activities on task queue `general` through two liminal connections:

| Activity | Node | Purpose |
| --- | --- | --- |
| `run_agent` | `agent` | Run or resume a Norn session with per-call instructions, schema, workspace, and tool policy. |
| `run_command` | `shell` | Execute one trusted command and return the completed process as data. |
| `parse_output` | `shell` | Extract JSON-path, regex, or matching-line data without terminal activity failures. |

The agent connection runs on a spawned OS thread. The shell connection runs on the main thread and writes the optional readiness file after that connection registers. Both use `serve_with_redial`, a shared stop signal, a `100 ms` initial delay, a `5 s` cap, and `usize::MAX` reconnect attempts. A terminal connection error stops the peer loop; the main thread always joins the agent thread and reports both diagnostics if both loops fail.

## Start manually

Build and start it from the repository root:

```sh
cargo build --manifest-path examples/general-worker/Cargo.toml
examples/general-worker/target/debug/general-worker \
  --address 127.0.0.1:50061 \
  --identity general-worker \
  --ready-file examples/general-worker/general-worker.ready \
  --norn-bin norn
```

This package is **manual-start only**. It installs no `launchd`, systemd, cron, or other persistent service definition.

CLI behavior:

- `--address <host:port>` is repeatable. With no occurrence, the only candidate is `127.0.0.1:50061`.
- `--identity <prefix>` defaults to `general-worker`; the two registrations use `<prefix>-agent` and `<prefix>-shell`.
- `--ready-file <path>` is optional. On shell registration, the worker writes the bytes `connected`. A write failure is logged loudly. This marker proves the shell connection registered; it is not a separate health assertion for a live Norn process.
- `--norn-bin <name-or-path>` overrides `NORN_BIN`; if neither is supplied, the default is `norn`.
- Unknown flags, missing values, blank values, a blank Norn binary, and a non-Unicode `NORN_BIN` fail startup.

## `run_agent`

### Input

```json
{
  "instructions": "required nonblank string",
  "prompt": "required nonblank string",
  "output_schema": "required nonblank JSON Schema string",
  "session_key": "optional string",
  "workspace_path": "required nonblank string",
  "disallowed_tools": "optional,comma-separated,string"
}
```

Missing fields, malformed activity JSON, non-UTF-8 input, and blank required fields are Norn-harness protocol errors. A supplied blank `session_key` is also a protocol error. An absent `session_key` is computed directly as `<workflow-id>-agent`. An absent or blank `disallowed_tools` value adds no deny-list argument. Caller-provided arguments are passed literally: substrings such as `{workflow_id}` and `{activity_type}` are not treated as templates. The sole transport normalization is that leading whitespace is removed from `output_schema`; all remaining schema content is preserved byte-for-byte.

Every Norn child receives these static arguments:

```text
--protocol jsonrpc --fast --reasoning-effort high
```

The per-run arguments are appended in this order:

```text
--append-system-prompt <instructions>
--output-schema <output_schema>
--session-id <session-key or workflow-id-agent>
--resume-if-exists
--workspace-root <workspace_path>
[--disallowed-tools <disallowed_tools>]
```

The schema is passed inline; no schema file is created. Norn `0.1.0` accepts inline JSON when the schema argument begins with `{`, so the worker removes insignificant leading whitespace before passing it. This does not expand reserved substrings or otherwise alter the schema. The worker enforces required/nonblank transport fields but does not reimplement JSON Schema validation; Norn owns interpretation of the supplied schema.

The wrapper replaces the neutral `AgentRunSpec` input with `prompt` encoded as a JSON string. Norn's terminal payload is returned verbatim—there is no post-run validation, reshaping, Git status check, commit, or other workspace mutation. The advertised intervention capabilities are exactly `InjectMessage` and `Cancel`.

### Sessions and resume behavior

The default session is isolated by workflow ID and reused by later `run_agent` calls in that workflow. Supplying the same `session_key` deliberately shares Norn history across calls (and potentially across workflows); callers own that trust and concurrency decision. `--resume-if-exists` resumes a matching session and otherwise starts it.

**Operator-confirmed behavior:** Norn applies `--append-system-prompt` when resuming an existing session. This package relies on that confirmed behavior so each call's `instructions` apply to both new and resumed sessions. The operator explicitly waived a duplicate real-binary resume experiment for `GPW-1`; no such experiment was performed as part of this implementation.

## `run_command`

### Input

```json
{
  "workspace_path": "/existing/working/directory",
  "name": "human-readable label",
  "argv": ["executable", "arg1", "arg2"]
}
```

`argv[0]` is resolved on the effective `PATH`; the remaining elements are passed directly as arguments without a shell. Empty `argv`, a missing executable, a dead working directory, or an operating-system spawn failure returns a terminal `ActivityFailure`. A child that starts and exits nonzero is a successful activity result with `passed: false`.

### Output

The output has exactly these fields:

```json
{
  "name": "human-readable label",
  "argv": ["executable", "arg1", "arg2"],
  "exit_code": 0,
  "passed": true,
  "stdout": "stdout only",
  "output": "stdout followed by stderr",
  "duration_ms": 12
}
```

On Unix, signal termination uses the shell convention `128 + signal`. `stdout` and combined `output` are clipped **independently** at 16,384 source characters, preserving a head and tail around an explicit marker such as:

```text
--- output truncated: 200 characters omitted ---
```

### Trust boundary

`run_command` is intentionally an arbitrary-command primitive. It is not a sandbox, allowlist, quoting layer, privilege boundary, or secret scrubber. Only trusted workflows should be allowed to dispatch to task queue `general`, and the worker process should run with the minimum filesystem and operating-system privileges appropriate for those workflows.

## `parse_output`

### Input and output

```json
{
  "text": "source text",
  "mode": "json_path | regex | lines",
  "query": "mode-specific query"
}
```

Every call returns data in this shape:

```json
{
  "ok": true,
  "value": "rendered value",
  "error": ""
}
```

On success, `value` contains the rendered value and `error` is the empty string. Bad input, a bad query, an unsupported mode, and a miss are never terminal activity failures. They return `ok: false`, an empty `value` string, and an `error` string that states whether JSON parsing, path traversal, regex compilation, regex matching, line matching, or mode selection failed:

```json
{
  "ok": false,
  "value": "",
  "error": "exact diagnostic"
}
```

Modes:

- `json_path`: parses `text` as JSON, then traverses dot-separated object keys and numeric array indices. An empty query addresses the root. String scalars return their raw string. Numbers, booleans, and `null` return JSON text. Arrays and objects return compact JSON text.
- `regex`: compiles `query` and uses the first match. If any named capture exists, `value` is a compact JSON object containing all named captures in deterministic key order; an unmatched optional capture is `null`. Otherwise, `value` is a compact JSON array of positional groups `1..N`, again using `null` for unmatched optional groups. A pattern with no explicit groups returns a one-element array containing the full match.
- `lines`: returns every source line containing `query`, in source order, joined with `\n`. No matching line is `ok: false`.

The handler uses no clock, randomness, environment data, or external state, so equal inputs produce equal outputs.

## Example workflow

`awl/general_worker_example.awl` invokes all three activities: it emits JSON with `run_command`, extracts the topic with `parse_output`, and asks a read-only `run_agent` session to summarize the workspace. Validate it with the repository-built CLI:

```sh
./target/debug/aion awl check examples/general-worker/awl/general_worker_example.awl
```

## Secrets

This worker reads no provider secret and never accepts a secret CLI flag. Each Norn child explicitly removes `OPENAI_API_KEY`, so an ambient key is not inherited; Norn can use the operator's configured ChatGPT OAuth login. Workflow inputs and arbitrary command output may still contain sensitive data, so operators must apply normal Aion authorization, retention, and logging controls.

## Open questions

None are required for `GPW-1`; the activity contracts and resume semantics above are explicit.
