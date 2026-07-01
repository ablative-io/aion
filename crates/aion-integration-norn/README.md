# aion-integration-norn

The first-party [Norn](https://github.com/ablative-io/norn) adapter for Aion — the first concrete
[`aion-integrations`](https://docs.rs/aion-integrations) `AgentHarness` integration.

It implements the harness-integration seam against a real Norn process driven in its
`--protocol jsonrpc` mode:

- `NornHarness` spawns `norn --protocol jsonrpc`, performs the `initialize` handshake, issues
  `run/execute`, and returns a live session.
- `NornSession` streams live `ActivityEvent`s (translated from Norn's `event/*` notifications),
  accepts capability-gated `InterventionCommand`s (mapped to `intervene/*` requests), and returns
  the id-matched `run/execute` Response as the terminal result.

This is the **only** crate in the Aion workspace that names Norn's on-wire contract. The aion
platform crates stay Norn-blind; Norn takes no dependency on aion. See the design pass
`docs/design/NORN-OBSERVABILITY-AND-INTERVENTION.md` (§3A.3, §3.4).

## License

AGPL-3.0-only.
