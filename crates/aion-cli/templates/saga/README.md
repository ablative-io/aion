# {{name}}

An order saga scaffolded by `aion new` from the `saga` template:

1. a `charge_payment` activity served by a remote worker, with a
   workflow-driven bounded retry loop over a durable backoff sleep,
2. a human `approval_decision` signal raced against a durable deadline,
3. an `order_status` query answerable at every stage,
4. saga compensation — rejection and timeout refund the captured payment via
   `refund_payment` and complete the order as `cancelled`.

## Layout

- `src/{{name}}.gleam` — the workflow. Edit the typed `handle` function and
  its helpers; the raw engine plumbing below the generated-code marker
  decodes/encodes JSON for you.
- `workflow.toml` — packaging descriptor read by `aion package`.
- `schemas/` — JSON Schemas for the workflow input and output.
- `aion.toml` — development server configuration.
- `worker/` — the Rust activity worker (present when scaffolded with
  `--worker rust`). It serves `charge_payment` and `refund_payment`.

## Run it

Build and package the workflow:

```sh
gleam build
aion package .
```

Start the server (terminal 1; state persists in `aion.db`):

```sh
aion server --config aion.toml
```

Start the activity worker (terminal 2). If this project was scaffolded with
`--worker rust`:

```sh
cargo run --manifest-path worker/Cargo.toml
```

Otherwise write one against a worker SDK — see the
[activities and workers guide](https://github.com/ablative-io/aion/blob/main/docs/guides/activities-and-workers.md).
The workflow dispatches `charge_payment` and `refund_payment` on the
`default` task queue.

Deploy and start a run (terminal 3):

```sh
aion deploy {{name}}.aion
aion start {{name}} --input '{"order_id":"ord-1","amount_cents":4999,"approval_timeout_ms":3600000}'
```

The worker charges the payment, then the run suspends waiting for the
decision. Watch it move with the workflow id that `aion start` printed:

```sh
aion query <workflow-id> order_status
```

You should see `"stage": "awaiting_approval"`. The wait survives restarts:
kill the server, start it again with the same config, and replay resumes the
run exactly where it was without re-charging the payment.

Approve to complete the order:

```sh
aion signal <workflow-id> approval_decision --payload '{"decision":"approved","approver":"ada"}'
aion describe <workflow-id> --pretty
```

`describe` shows `"status": "Completed"` with `"status": "completed"` in the
result. Send `{"decision":"rejected","approver":"ada"}` instead — or let the
deadline lapse — to watch the compensation path: the worker refunds the
payment and the order completes as `cancelled` with a `refund_id`.

## Next steps

- Make `charge_payment` fail transiently (`ActivityFailure::retryable`) in
  the worker to watch the workflow-driven retry loop and durable backoff.
- [Workflow authoring guide](https://github.com/ablative-io/aion/blob/main/docs/guides/workflows.md)
  — child workflows, timers, determinism rules.
