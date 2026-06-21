# Authoring activities: declare once, generate the rest

Aion authoring follows one rule (ADR-014): **the typed Gleam declaration is the
single source of truth.** You declare an activity once — its name, its tier, and
its typed input and output — and `aion generate` derives everything that must
agree byte-for-byte across the workflow, the codecs, the worker, and the wire
goldens. There is no separate DSL or manifest to keep in step, and a hand-edit
to any generated file is a build error.

This guide walks the model end to end using `examples/order-saga`.

## The model

For each activity you write exactly two things:

1. **The declaration** — one entry in your package's `manifest()`.
2. **The body** — the side-effecting code that runs the activity (a Gleam
   function for the in-VM test double, and/or a handler in your worker for the
   remote tier).

Everything else is generated:

| Generated file | What it is |
| --- | --- |
| `src/<package>_io.gleam` | The value types and their JSON codecs, from `schemas/*.json`. |
| `src/<package>_codecs.gleam` | One typed `Codec` per value type, wrapping the io encoders/decoders. |
| `src/<package>_activity_wrappers.gleam` | One `activity.new(...)` constructor per declaration, pairing the engine name, codecs, and body. |
| `worker/worker.py` or `worker/src/main.rs` | Worker plumbing that decodes each task and routes it to your handler. |
| `test/<package>_wire_compat_test.gleam` | A wire-compat golden per remote value type, pinning its encoded shape. |
| the `activities` list in `workflow.toml` | The declared activity names, in declaration order. |

Generated files carry a `do not edit` header. Run `aion generate <project>` to
(re)write them, and `aion generate <project> --check` in CI: it regenerates
every file in memory and exits non-zero if any on-disk copy has drifted.

## 1. Describe the value types as schemas

Each activity input and output value type is a JSON Schema under `schemas/`. The
file stem becomes the Gleam type name (`order_input.json` → `OrderInput`). The
supported subset is objects (with `required` and `additionalProperties: false`),
string enums, arrays, and the scalar types; see
[`docs/guides/codegen.md`](guides/codegen.md) for the full table.

```json
// schemas/order_input.json
{
  "type": "object",
  "required": ["order_id", "item", "quantity", "amount"],
  "additionalProperties": false,
  "properties": {
    "order_id": { "type": "string" },
    "item": { "type": "string" },
    "quantity": { "type": "integer" },
    "amount": { "type": "integer" }
  }
}
```

## 2. Declare each activity

In `src/<package>_activities.gleam`, write the bodies and the `manifest()`. The
declaration captures the typed input and output through `activity.type_ref`, so
an inconsistent declaration fails `gleam build` — the contract is checked, not
asserted.

```gleam
import aion/activity
import aion/error
import aion_order_saga_codecs as codecs
import aion_order_saga_io as io

pub fn reserve_inventory(
  input: io.OrderInput,
) -> Result(io.InventoryReservation, error.ActivityError) {
  Ok(io.InventoryReservation(
    order_id: input.order_id,
    reservation_id: "res-" <> input.order_id,
    item: input.item,
    quantity: input.quantity,
  ))
}

pub fn manifest() -> List(activity.Declaration) {
  [
    activity.declare(
      "reserve_inventory",
      activity.RemotePython,
      activity.type_ref("OrderInput", codecs.order_input_codec()),
      activity.type_ref("InventoryReservation", codecs.inventory_reservation_codec()),
    ),
    // ... one per activity ...
  ]
}
```

The tier is one of `activity.InVm`, `activity.RemotePython`, or
`activity.RemoteRust`. No retry policy, timeout, or backoff appears in a
declaration unless you add one (ADR-001): absence is intentional, so codegen can
never bind you to a policy you did not choose.

Declaration order is load-bearing — it fixes the order of the generated
wrappers, the worker registry, and the `workflow.toml` activities list — so a
byte-identical round-trip depends on it.

## 3. Generate the plumbing

```sh
aion generate examples/order-saga
```

`aion generate` runs your `manifest()` through the Gleam toolchain to read the
declarations, then writes the io, codecs, wrappers, worker, and golden files and
syncs the `workflow.toml` activities list. (To do this it builds your package;
modules that import the generated wrappers are set aside for the build and
restored afterward, so a fresh project — with no wrappers yet — still generates
cleanly.)

## 4. Use the wrappers in your workflow

The workflow calls the generated typed constructors — never `activity.new`
directly:

```gleam
import aion_order_saga_activity_wrappers as wrappers

case workflow.run(wrappers.reserve_inventory_activity(input)) {
  Ok(reservation) -> // ...
  Error(activity_error) -> // ... compensate ...
}
```

## 5. Write the worker bodies

For a remote tier, the generated `worker/worker.py` is plumbing only — it
decodes each task and routes `task.activity_type` to a handler of the same name
in the hand-written `worker/handlers.py`:

```python
from aion_worker import Completed, DispatchOutcome

async def reserve_inventory(request: dict[str, object]) -> DispatchOutcome:
    order_id = request["order_id"]
    return Completed(json_payload({
        "order_id": order_id,
        "reservation_id": f"res-{order_id}",
        "item": request["item"],
        "quantity": request["quantity"],
    }))
```

The `RemoteRust` tier is symmetric: the generated `worker/src/main.rs` defines
the `serde` structs and registration and calls `handlers::<name>` in
`worker/src/handlers.rs`. The in-VM tier needs no worker — the Gleam body the
wrapper references is the whole activity.

## 6. Keep it honest in CI

```sh
aion generate examples/order-saga --check
```

Run this in CI. If anyone hand-edits a generated file or a declaration drifts
from the generated output, `--check` exits non-zero and names the file. Deleting
every generated file and re-running `aion generate` reproduces them
byte-identically, so the generated tree is always a pure function of your
declarations and schemas.

See [`examples/order-saga/README.md`](../examples/order-saga/README.md) for the
full run-it-locally walkthrough, including the worker and the compensation
paths.
