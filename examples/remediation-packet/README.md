# remediation-packet: a findings packet as one durable Aion run

A packet of lane briefs goes in; every lane lands on the base branch or is
honestly reported unlandable. A driven coordinator agent (Norn, resumable
session) plans waves, dispatches one `lane_run` child per lane, judges and
merges the results, and escalates deep-tear lanes to the reviewing operator,
who answers with the `ruling` signal. The packet itself — manifest, charter,
lane briefs — is committed files, not workflow input: the run carries only a
`PacketConfig` pointing at them, so the repo stays the single source of truth
and the workflow stays generic across packets.

## Composition

```
remediation_packet                      (awl/remediation_packet.awl)
│
├── select        (coordinator agent turn: read charter/manifest/briefs,
│   │              plan, emit the next wave — zero entries ⇒ close;
│   │              carries the `max config.max_waves visits` cycle bound)
│   ▼
├── build_wave    fork entry in wave.entries
│   │               lane_run(...)       (child, one per lane, CONCURRENT)
│   │             join -> lane_results
│   │             land_wave (coordinator turn: judge each branch at the
│   │             diff, merge small lanes one at a time, wave-tail battery,
│   │             push green, post meridian; escalations → hold_ruling,
│   │             rework → next select, halt → wrap_halt)
│   ▼
├── hold_ruling   wait ruling            (durable block on the operator)
│                 apply_rulings (merge approved, queue changes as rework)
│                 → back to select
│
├── close         (coordinator turn: reconcile final ledger, post closing
│                  report) → finished
└── wrap_halt     → halted (the coordinator refused to push past something)

lane_run                                (awl/lane_run.awl — dev_brief
│                                        repackaged: exhaustion is a
│                                        DISPOSITION on success, never a
│                                        workflow failure)
└── review_round                        (awl/review_round.awl — one
                                         adversarial lens per child)
```

## Workers

Two existing worker binaries serve everything — no new worker:

- **`examples/dev-brief/worker`** (task queue `dev_brief`, nodes
  `shell`/`developer`/`reviewer`) serves every `lane_run`/`review_round`
  activity unchanged — it routes by queue/node + activity type + JSON shape,
  never by workflow name.
- **`examples/general-worker`** (task queue `general`, nodes `agent`/`shell`)
  serves the coordinator turns. The four coordinator actions are
  **agent-activity aliases** of `run_agent` — same harness, same input
  contract, distinct names so the workflow declares each with its own typed
  result:

  ```sh
  general-worker \
    --address <server-host>:50061 \
    --agent-activity select_wave \
    --agent-activity land_wave \
    --agent-activity apply_rulings \
    --agent-activity close_packet
  ```

The coordinator session is `{workflow_id}-coordinator` for all four actions
(`--resume-if-exists`), so one agent carries the packet end to end. Lane
developer sessions are `{lane workflow_id}-developer`, reviewers per-lens per
child — the dev-brief worker's session model, unchanged.

## Deploy and start

```sh
aion awl check awl/remediation_packet.awl awl/lane_run.awl awl/review_round.awl
aion deploy awl/review_round.awl        # children first: the parent binds
aion deploy awl/lane_run.awl            # deployed names at spawn time
aion deploy awl/remediation_packet.awl
aion start remediation_packet --input-file packet-config.json
```

`packet-config.json` is one `config` object (see `PacketConfig` in the AWL:
`repo_root`, `packet_dir`, `base_branch`, `gates`, `verify_gates`,
`max_fix_cycles`, `lenses`, `max_waves`, `meridian_as`, `meridian_to`).
`packet_dir` is a repo-relative or absolute path to the packet: `PACKET.md`
(lane manifest), `coordinator-brief.md` (the coordinator's charter — the
workflow's every agent turn defers to it), and `lanes/*.md` (one dev-brief
input per lane). The first packet lives at `docs/remediation/2026-07-23/` in
this repository.

## The ruling signal

Deep-tear escalations durably block the run until the reviewing operator
answers:

```sh
aion signal <run-id> ruling --payload '{"rulings":[
  {"lane_id":"rem-l06-positional-now","decision":"approve","notes":""},
  {"lane_id":"rem-l07-haematite-outbox-cas","decision":"changes",
   "notes":"the claim guard enumeration missed the reconciler path - see meridian"}
]}'
```

`decision` is `approve` (the coordinator merges, battery, push) or `changes`
(the lane loops back as rework carrying the notes verbatim). Escalated lanes
absent from a batch stay held and are re-escalated.

## Trust boundaries, stated honestly

- The coordinator **echoes** each lane's dev-brief input from the lane file
  (slimmed per its charter: summary objective + mandatory read-first pointer
  at the committed file). The lane file is authoritative; the deep-tear
  reviewer checks the ran brief against it.
- Lane children never push; only the coordinator touches the base branch, one
  merge at a time, and only after its own read of the diff — the lenses are
  advisory to it, not a substitute.
- Nothing here parses gate output for truth: gate exit codes are recorded
  data, and the battery re-runs at the wave tail before any push.
