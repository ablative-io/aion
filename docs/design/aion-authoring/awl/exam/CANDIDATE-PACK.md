# AWL — candidate pack

This is everything you get. Read it, then complete the task at the bottom.

## The language in one page

AWL is a small declarative language for durable workflows. A `.awl` file
declares a workflow's inputs and outcomes, the types that move through it,
the worker actions it calls, and the steps that wire them together. The
engine executes it durably: every action call, timer, and loop iteration is
recorded and survives restarts.

A complete, real workflow:

```awl
//! Greet a name, then shout it.
workflow awl_hello
  input name: String
  outcome shouted: type Shouted, route success

type Greeting { greeting: String }
type Shouted  { text: String }

worker awl_hello
  action greet(name: String) -> Greeting
  action shout(text: String) -> Shouted

step greet_and_shout
  name |> greet |> .greeting |> shout |> route shouted
```

The pieces:

- **workflow header** — `workflow <name>`, then `input <name>: Type` lines
  and `outcome <name>: type <Type>, route success|failure` lines. Outcomes
  are the workflow's possible endings; every run finishes by routing to
  exactly one.
- **types** — nominal records: `type Order { id: String, status: String }`.
  Optional fields take `?`: `note: String?`. Lists: `[String]`. Primitives:
  `String`, `Int`, `Float`, `Bool`.
- **worker** — `worker <name>` (the task queue), then one `action` per
  callable: `action fetch(id: String) -> Order` followed by an optional
  config line, e.g. `node shell, timeout 5m, retry 2 every 30s`. `retry N
  every D` means N retries spaced D apart, on top of the first attempt.
- **steps** — `step <name>` or `step <name> after <other>, <another>`.
  Inside a step: action calls `fetch(id: order_id) -> order` bind results
  to names visible in all later steps; pipelines chain with `|>` and field
  access is `.field`; `route <outcome>(field: value, ...)` ends the run.
- **conditional outcomes** — inside a step:
  `outcome done: when order.status == "delivered", route success_outcome(...)`
  and `outcome other: otherwise, route failure_outcome(...)`. A route target
  can be a workflow outcome (with its payload) or the name of another step —
  `route handle_failure` continues the run at that step.
- **durable time** — `sleep 24h` is a durable timer statement (the workflow
  parks; no resources are held). Durations: `30s`, `10m`, `24h`.

Check your file with:

```
aion awl check <file>.awl
```

It typechecks everything: every route target exists, every call matches its
action signature, every outcome payload matches its type. Fix what it
reports until it passes.

## The task

Write a workflow named `order_followup` in a single file
`order_followup.awl`. It receives an order id (`String`) as input, and it
must:

1. Fetch the order — an action that takes the id and returns an order
   carrying at least a status field. Give this action a 1 minute timeout
   and 3 retries spaced 10 seconds apart.
2. Send a confirmation notification for the order (an action; its shape is
   yours to design).
3. Wait 24 hours.
4. Fetch the order again.
5. If the refreshed order's status is `"delivered"`, finish with an outcome
   named `done` (route success) carrying the order id.
6. Otherwise, call an escalation action and finish with an outcome named
   `escalated` (route failure) carrying the order id and the status you saw.

Design your own types, worker, and action signatures — the task fixes WHAT
happens, you decide the shapes. When `aion awl check` passes and you are
satisfied it does what the task says, you are done.
