# aion-integration-cli

A **second, independent** [`aion-integrations`](https://docs.rs/aion-integrations) `AgentHarness`
integration for Aion — the empirical proof that the harness-integration boundary is a real, neutral
SDK, not a Norn wrapper (NOI-8).

`aion-integration-norn` is the first adapter (a rich, bidirectional JSON-RPC harness). This crate is
a second adapter of a **deliberately different shape**, so two implementations of one trait exist
across two crates. Both fit the neutral seam without any change to `aion-core`, `aion-integrations`,
the wire, the server, or the worker.

## The two shapes

- **`CliHarness`** — the observability-only case. A plain-stdout CLI agent with **no control channel
  at all**: it spawns a line-oriented process and demuxes its interleaved stdout into neutral
  `ActivityEvent`s (mostly `Raw`, some mapped). It advertises an **empty** intervention capability
  set, so `intervene` rejects every command and the ops console offers no controls for it — a
  first-class tier, not a degenerate one.
- **`MockAgentHarness`** — the interveneable case. A deterministic in-crate harness advertising
  `{inject_message, cancel}` that accepts those and cleanly rejects the primitives it does not
  advertise (notably `respond_to_approval`) with a capability-not-supported NACK. It exercises the
  other branch of the `intervene` contract, driven live by the worker trait driver
  (`aion_worker::spawn_agent`).

There is **no Norn here** — this crate exists precisely to prove the contract is neutral. See the
design pass `docs/design/NORN-OBSERVABILITY-AND-INTERVENTION.md` (§3A.1, §9.1 NOI-8).

## License

AGPL-3.0-only.
