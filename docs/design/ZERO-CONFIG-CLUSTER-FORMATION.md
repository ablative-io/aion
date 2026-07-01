# Zero-config cluster formation via self-form + haematite merge-on-discovery

> DESIGN BLUEPRINT. Synthesised from six read-only analyst reports (merge, epoch,
> membership, conflict, discovery, competitive), each grounded in real
> `aion`/`haematite`/`beamr` source on `main`. This doc does not modify any repo.
> It is written in the house style: source-grounded, decisions-with-consequences,
> gates before build. Where the analysts disagreed with the originating thesis, the
> doc sides with the source, not the pitch.
>
> Status: DESIGN — not approved to build. Several load-bearing steps are marked
> **SPIKE-FIRST**; they must pass a named negative-control before any implementation.

---

## 0. The one-paragraph honest summary

Two independently-genesised haematite clusters CAN be fused into a single
deterministic content-addressed root — the union merge (`merge_committed_union`,
`handoff_merge.rs:121`) is genuinely ancestor-free, history-independent, and
proptest-pinned. This is a real capability no surveyed incumbent has (Serf merges
membership but corrupts the Raft log; Akka downs a side; Cockroach refuses at a
permanent cluster-ID check). **BUT** the union is a per-key `max(epoch, seq)`
last-writer-wins pick, and its *correctness* rests on the R-LE invariant
(`shard/actor.rs:1140-1142`) that holds only WITHIN one epoch-fenced lineage. Across
two independent geneses that invariant is broken: for a key written on BOTH sides,
the merge **silently drops one committed, client-acked write** by node-id
lexicographic accident. Therefore this design ships **self-form + auto-CONVERGE**,
not "lossless auto-merge." The withhold-external-writes serving gate is not polish —
it is the correctness precondition that keeps the conflicting-write set empty so the
union stays in its provably-clean regime (disjoint / one-sided / idempotent). The
residual — a partition straddling the settling window while clients drive the SAME
`workflow_id` on both halves — is not eliminable by timing (FLP), and this design
handles it by **detecting and quarantining** the forked run loudly, never by silent
splice. Single-node is trivially and genuinely zero-config today.

---

## 1. Goal, and the fundamental limit it accepts

### 1.1 Goal

- **Zero flags.** No `--bootstrap`, no `--cluster-size`, no `--expected-size`. The
  operator runs `aion server` on each node with a shared cookie and nothing else.
- **Single-node is trivially correct.** A lone node self-quorums (`quorum_size(1)=1`,
  local ack self-satisfies, `consistency.rs:76-79,281-287`). This is a first-class
  valid config TODAY and needs nothing beyond #146's write-and-read path. **This
  claim is true and grounded** (membership.md Q1).
- **Multi-node self-forms then converges.** N co-booting nodes sharing a cluster
  identity `cn = hash(cookie)` discover each other, and either JOIN an already-formed
  cluster or CO-FORM one; nodes that genesised independently before discovery MERGE
  (converge) into one deterministic root instead of a permanent split.

### 1.2 The fundamental limit this design ACCEPTS (states plainly)

Incumbents force a genesis prior to *prevent* split-formation because their state is a
position-dependent log with no merge function; a permanent split is the only
alternative to prevention (competitive.md §1–§4). haematite removes the *structural*
blocker — a split becomes a convergent single root, not a permanent fork. It does
**not** remove the *semantic* limit:

> **Same-key / same-shard writes accepted concurrently on two sub-clusters before they
> merge cannot be reconciled losslessly by any known strongly-consistent system, and
> haematite is no exception.** The union merge resolves them by deterministic
> `max(epoch,seq)` order, which across independent geneses is arbitrary (node-id
> tiebreak), i.e. silent LWW data-loss unless prevented.

The design's safety claim is therefore precise and bounded:

> **"No conflicting writes during formation," delivered by the serving gate.** The
> merge auto-heals the non-conflicting remainder (the overwhelming majority: disjoint
> keys, one-sided keys, idempotent replays). It is a *convergence* mechanism, not a
> *conflict resolver*. For the residual conflicting case that survives the gate, the
> system fails LOUD (detect + quarantine), never silent.

This is strictly better than a permanent split (unrecoverable) or a downed side
(availability loss), and it is honest about the one wall no one has beaten.

---

## 2. The node formation state machine

There is **no formation state machine today** (discovery.md §1). The server binds and
serves the instant `serve_grpc`/`serve_http` are called (`run.rs:221-222`). This is
entirely new control flow inserted between `ServerState::build` (`run.rs:161`) and the
transport spawns.

```
BOOT
  store connected (connect_haematite_store); replication endpoint BOUND
  (must be reachable so peers can discover & sync); client mutation surface GATED-CLOSED
    │
    ▼
DISCOVERING  ── register _aion._tcp.local. (nid/repl/grpc/cn/pv); browse; cn/pv/self-filter
  │            accumulate cn-matched, cookie-authenticated candidate dial addrs
  │            SERVING GATE CLOSED (client mutations return retryable "forming")
  │
  ├─(A) a discovered candidate ALREADY holds a durable cluster/members record
  │        naming a live set I am not in → JOIN-EXISTING (safe; NO self-genesis)
  │        propose `add self (Joining)` CAS against the CURRENT denominator
  │
  ├─(B) window T elapsed AND candidate set stable for a quiescence sub-window AND
  │      NO candidate holds a members record → CO-FORM-FRESH
  │        deterministic min(node_id) election over {self} ∪ candidates
  │        winner writes genesis members; losers re-poll → JOIN-EXISTING
  │
  └─(C) window T elapsed, ZERO candidates → SELF-FORM-ONE
           denominator-1 self-quorum (the trivial, genuinely-safe zero-config path)
           later peer discovery → MERGE-ON-DISCOVERY (§3) — NOT failover (§5)
    │
    ▼
FORMATION-SETTLED  (per-shard: a single owner exists whose epoch dominates)
    │
    ▼
SERVING  open the serving gate → client mutation surface admits writes
```

### 2.1 The serving gate (load-bearing, buildable, clean)

- **Bind early, admit late.** Do NOT delay the transport bind (delaying makes the port
  `ECONNREFUSED`, which clients retry blindly and which breaks health checks).
  Bind immediately as today; have mutation handlers return a typed retryable
  `Unavailable`/`FailedPrecondition` ("cluster forming, retry") while `Forming`.
  Precedent: the deploy service already answers `Unimplemented` when dark
  (`run.rs:275-279`); the shutdown path already threads a `watch::channel(false)`
  through both transports (`run.rs:188`). Mirror `ShutdownState` (`shutdown.rs:64`)
  with a symmetric `FormationState { Forming, Ready }` on `ServerState`.
- **Gate mutations ONLY, never reads/replication/health/console.** Gate
  `start_workflow`, `signal`, `cancel`, deploys that mint state. Do NOT gate:
  replication traffic (peers MUST sync during formation — gating it deadlocks
  formation), health/readiness probes, `query` on existing state (a read creates no
  merge-conflicting history), the ops-console sensor surface (operators must WATCH
  formation).
- **INTERNAL writes matter too (epoch.md §4).** The window must also hold back
  *internal* history-creating writes for the same shard — timers firing, workflow
  state advancing on already-admitted work — because those are exactly the same-shard
  writes that collide. The gate is "no NEW run may begin and no admitted run may
  advance to a shard whose ownership is not yet settled," not merely "no client RPC."

### 2.2 The gate is a probability reduction, NOT a correctness proof (be blunt)

Even a perfect gate does not make the co-form path *correct*. It shrinks the window in
which two co-forming halves both accept writes to `(window − first-write-latency)`. If
mDNS never delivers a peer (multicast dropped/blocked), a node self-forms-one (state C)
and a peer appearing LATER is a merge with a possibly non-empty conflicting-write
history. **The gate makes the common case safe and the demo clean; it does not close
the FLP hole** (discovery.md §2.3, §4). That hole is closed only by a prior
(`--bootstrap`) or a real pre-write quorum agreement that does not exist. This design
chooses to accept the residual and make it LOUD (§4), rather than reintroduce a prior.

---

## 3. The MERGE protocol, grounded in the real machinery

The thing the thesis calls "just a merge" — a **two-live-cluster union** — is UNBUILT
(discovery.md §5.4). `adopt_shards` + `merge_committed_union` today handle only
*intra-cluster owner handoff* (a new owner folds its promise-majority's roots, driven
from `become_live`, `receiver.rs:944-989`). There is no code path that merges two
independently-live clusters (epoch.md §0, grep of `merge_adopt` callers). The merge
protocol has three reconciliation axes, and they must run in a specific order.

### 3.1 The reusable primitive and its exact behaviour

`merge_committed_union(root_a, root_b, store)` (`handoff_merge.rs:121-176`):

- Collects every stored `(key, stamped-bytes)` from both roots (`merge_root_into`,
  confirmed `handoff_merge.rs:149-176`), folds by per-key `max` over `(epoch, seq)`,
  then rebuilds ONE history-independent tree via `batch_mutate`.
- `stamp > existing` → descendant wins (value or tombstone); `stamp == existing &&
  bytes != existing` → **`DuplicateStamp` hard error**; `stamp < existing` or
  equal-identical → keep incumbent (idempotent).
- Commutative, associative, idempotent — a genuine bounded semilattice join,
  proptest-pinned (`handoff_merge_proptest.rs`). History-independence proven at both
  tree and merge layers (300-case proptests + pinned minimal-counterexample regression,
  `mutate_history_independence_tests.rs`). **This part has no gap** (merge.md §4).
- The CAS `expected` hash never enters the merge; it operates purely on stored stamped
  bytes. The per-stream append CAS that normally guards gaps does NOT run during merge
  (merge.md §5.3) — this is why append-structured data is the sharpest edge.

### 3.2 Axis 1 — MEMBERSHIP reconciliation (needs a NEW resolver; union merge cannot do it)

`cluster/members` is a **single key**. Cluster A holds `cluster/members → {A}@stampA`,
B holds `cluster/members → {B}@stampB`. `merge_committed_union` keeps only
`max(stampA, stampB)` → `{A}` OR `{B}`, **never `{A,B}`** (membership.md Q2). The
losing cluster's entire member set is dropped. This is the exact opposite of what
merge-on-discovery needs.

**Decision:** membership merge is a **value-semantic set-union resolver**, keyed on
matching `cn`, living OUTSIDE the generic (value-blind) union merge. On discovering a
same-`cn` peer with a different genesis, compute the set-union of the two member lists
and emit a new record with a fresh, strictly-higher config `epoch`.

**Consequence / SPIKE-FIRST:** the denominator transition across a merge has NO
overlapping-majority safety proof. #146's single-change (±1) CAS is provably safe
because old- and new-majorities overlap; a MERGE of two disjoint self-quorums (each is
its own majority) has NO overlap (membership.md Q2). Two options:
  - **(a) reduce merge to a sequence of single-change JOINs** driven by ONE canonical
    survivor after a deterministic "which record is canonical" election — each JOIN is
    then a provably-safe +1. This is the realistic path but REQUIRES a canonical-record
    election that does not race.
  - **(b) prove a new joint-consensus-style overlap property** for the two-disjoint-
    quorum fusion.
  This design picks **(a)** and marks the canonical-record election **SPIKE-FIRST**
  (§6, GATE-MERGE-SAFE-DENOM).

### 3.3 Axis 2 — EPOCH-LINEAGE reconciliation (reuse AcquireShard to depose one side)

Merging DATA alone deposes no one — `merge_adopt` never touches `promised`,
`owner_epoch`, or `persisted_max_minted` (epoch.md §2, confirmed: `merge_adopt` body
folds committed data trees only). Post-merge each node still carries its own lineage's
`promised`, so you get either a luck-of-node-id depose or a persistent two-owner split.

**Decision:** after a merge is decided, run a **forced per-shard re-election over the
UNION membership** before serving that shard. The primitive is reusable AS-IS:
- `run_prepare_round` over the combined send-targets (`endpoint.rs:754`).
- R4 `persisted_max_minted` (`actor.rs:1035-1047`) guarantees the new ballot exceeds
  BOTH lineages' prior ballots on any participant — so ONE new Prepare at
  `max(all seen)+1` deposes both old owners via the existing fence
  (`actor.rs:701-706`). This is a genuine deterministic depose: the higher-epoch owner
  wins, and its first write `(live_epoch, 0)` then dominates every merged entry,
  restoring R-LE for all writes made AFTER the merge.

**What must be BUILT (new machinery, epoch.md §3):**
  1. **A discovery-driven trigger** — nothing calls `acquire_shard` on "I discovered a
     same-`cn` peer with a different genesis." All election today is failover-driven.
  2. **A union-membership `WriteMembership`** re-derived over the union set (the
     denominator changes N,M → N+M). This is the Axis-1 output.
  3. **A quiesce gate** that holds writes to each shard until its forced re-election
     completes — gating on the re-election, NOT wall-clock (else case-2 split-brain
     writes sneak in during the window).
  4. **Ordering:** the winner must Prepare (depose both olds) BEFORE it merges+serves,
     else a still-live loser commits a fresh write racing the merge. This is the SAME
     ordering discipline `aion/src/engine/fence.rs` already enforces for failover
     (acquire→publish before extend/recover, `plan_adopted_shards:82-104`) — reusable
     conceptually; the cross-cluster trigger is new.

### 3.4 Axis 3 — DATA merge (the union, gated by the divergence detector)

For every key NOT on the `E`-stream and NOT `cluster/members`, `merge_committed_union`
is correct and lossless in its clean regime: disjoint keys (set union), one-sided keys
(kept verbatim incl. tombstones), idempotent replays (kept-once). This is the bulk of
state and it genuinely auto-heals.

For `E`-stream keys (workflow histories) the union's per-key max-pick is the WRONG unit
(§4). **Decision:** the DATA merge does NOT blindly call `merge_committed_union` over
`E`-stream ranges. It first runs the divergence detector (§4); on a clean
prefix/extension it takes the plain union (longer supersedes); on a genuine fork it
refuses to splice and quarantines (§4). All other keyspaces go straight through the
union.

**Mechanical preconditions of the merge itself (merge.md §6):** every node reachable
from BOTH roots must be present locally (a full-tree pull, hash-verified, not a diff);
every committed entry on both sides must be a valid stamped envelope (both clusters
must run a stamped-entry-era build); the adopt is a single durable in-slice commit with
a Tier-0 dir-fsync barrier before the WAL marker (`actor.rs:1179-1197`), so a crash
mid-merge never leaves a torn baseline.

---

## 4. Resolution policy for the same-workflow divergence hard case

### 4.1 Why per-key max-pick is categorically wrong here

A workflow history is an append-only per-`workflow_id` log at tree keys
`E || uuid || 0x00 || seq` (confirmed `event_store.rs:11-18`; aion side
`keyspace.rs:45-50`). `seq` is aion's OWN app-level contiguous event sequence — it IS
the tree-key component (conflict.md §1.2). Two sub-clusters that each appended a
DIFFERENT event #N for the same `workflow_id` write the byte-IDENTICAL key
`E||uuid||0x00||N`. The union then:
- picks ONE side's bytes per position by `max(epoch,seq)` — and it is **per-key
  independent**, so event #3 may resolve to A and event #4 to B, producing an arbitrary
  interleave that is a THIRD history neither cluster ever ran;
- the per-stream CAS scalar and seq-counter are ALSO max-merged, so the advertised head
  can be consistent with EITHER length, decoupled from which bytes survived.

aion's replay contract forbids this: one run has exactly ONE history; a positional/
anchor mismatch is a typed `NonDeterminismError` (`resolver.rs:101-145`), and workflow
`now` advances only from recorded event timestamps (`determinism.rs:25-53`), so an
event-position swap changes downstream decisions. A byte-clean tree replays into a
failed run at best, a plausible-but-never-executed run at worst. **The correct
resolution unit is the whole RUN, not the event key.**

### 4.2 The detector (designable now — this is the honesty gate)

The `DuplicateStamp` guard does NOT fire on the cross-cluster case: two independent
geneses have different `node` tiebreaks, so their stamps differ and the merge sails
through as a silent max-pick (conflict.md §2.2, competitive.md §5.2). There is no
existing "two committed heads at the same aion-seq" detector; it must be built at the
aion layer, above haematite.

**Decision — a new aion-layer pre-merge pass:** for each `E`-stream present on BOTH
committed roots, walk both ranges and find the lowest `seq` where the committed payload
bytes diverge (the fork point). The raw material is present and iteration already
exists (`collect_stored_entries`-style, `handoff_merge.rs:189-218`).
- **No divergence (clean prefix/extension):** the streams are identical or one is a
  strict extension of the other → the longer supersedes; plain union is safe. This is
  the common benign case (one side simply made more progress). Note: a workflow that is
  deterministic given identical inputs produces byte-identical events on both halves →
  idempotent keep-one, NO divergence (conflict.md §5). The residual bites only when a
  run is input/timing-sensitive AND both halves drove it independently.
- **Genuine fork:** STOP. Do not call the union merge for this stream. Fail closed.

### 4.3 The resolution (fork case) — quarantine, never splice

1. **Pick ONE surviving history by the cluster total order (higher-epoch owner wins the
   run verbatim).** Because the winning cluster's forced-re-election `live_epoch`
   strictly exceeds every merged write's epoch (R-LE restored post-re-election), this
   is consistent with the storage layer's own ordering and is deterministic.
2. **Quarantine the loser's divergent tail — do not merge it in.** Record a durable
   "run X superseded at fork seq k; divergent tail preserved under a new quarantined
   run-id." This mirrors aion's terminal-failure discipline (`fail_on_violation` writes
   a terminal event rather than corrupting, `resolver.rs:184-195`): the loser run gets
   a terminal `superseded` event; its work is re-parented, never interleaved.
3. **Do NOT attempt "quorum-of-original-write."** Neither write was a quorum of the
   MERGED cluster; there is no shared quorum to appeal to. The tiebreak MUST be the
   deterministic total order, not a re-vote (conflict.md §4).

### 4.4 The honest residual risk after the gate

- **Common case → ZERO divergence.** If the cluster forms before any client connects
  and the gate withholds writes until settled, no run is ever driven on two halves;
  `E`-streams are identical or empty; the union is a trivial clean union. This is the
  strong, honest win, and `NotOwner` back-pressure already exists to hold/retry writes
  (`store.rs:1444-1447`).
- **Residual → a partition straddling the window while clients drive the SAME
  `workflow_id` on BOTH halves with divergent non-deterministic choices.** Not
  eliminable by timing (FLP). This design handles it by **detect + quarantine + alert**,
  fail-closed. It is strictly better than silent max-pick splice.
- **Side effects on the losing half are UNRECOVERABLE by history reconciliation
  (SPIKE-FIRST).** The loser may have charged a card / sent an email. Quarantine +
  re-parent + operator compensation is a *policy*, not an automatic guarantee. Defining
  it well — what a `superseded` terminal event is; how the loser's children, timers,
  signals are handled; how LOCAL-only outbox rows (`store.rs:1266-1267,1343-1345`)
  reconcile; that the fork is graph-shaped across shards, not per-stream — is genuinely
  open design (conflict.md §6). **Automatic correct resolution of a divergent run is a
  research spike; this design ships detect+quarantine only and defers auto-resolution.**

---

## 5. What #146 must provide, and composition with failover/adoption

### 5.1 #146 is the substrate and it is UNBUILT

`sync/membership.rs:58` still returns `config.nodes.len()` (confirmed
`resolve_membership`); there is no `cluster/members` record, no `SeedProvider`, no
`propose_membership_delta`, no dynamic denominator. The good news that makes this
buildable: **the denominator is already a per-write value** (`&WriteMembership` threaded
into `replicate_write`, `receiver.rs:84-92`), not a baked constant — a durable record
CAN feed it (membership.md Q1). What #146 must deliver for merge-on-discovery to stand
on (membership.md Q4), most load-bearing first:

1. **A membership-specific set-union resolver** (Axis 1, §3.2) — `merge_committed_union`
   cannot be reused for `cluster/members`.
2. **A stable `cn = hash(cookie)` PERSISTED in the record**, plus a merge rule keyed on
   matching `cn`, so the resolver knows "these two records are two views of one cluster
   to union" vs "two unrelated writes, keep newer."
3. **A denominator-transition protocol whose safety proof covers a MERGE** (§3.2
   option (a): merge = sequence of single-change JOINs after a canonical-record
   election). **SPIKE-FIRST.**
4. **A resolution for conflicting shard writes across disjoint epoch spaces** (§4).
   This is the deepest gap; the union merge does NOT solve it.
5. **A per-write denominator source reading the durable record (CSOT-1)** with defined
   precedence (durable record wins over static `config.nodes`). #146 already designs
   this; it is the easy, inert-and-safe part.
6. **A withhold-external-writes gate wired to the formation state machine** (§2.1).
   Buildable; must not be sold as making the pre-merge window safe.

### 5.2 MERGE must NOT be read as FAILOVER (the sharpest integration risk)

The two events are structurally distinct: FAILOVER fires on *loss* of a KNOWN peer
(`peer_connected` → false past debounce, `cluster.rs:9-27,243-256`); MERGE fires on
*appearance* of a previously-UNKNOWN peer. Today the confusion risk is LOW because the
supervisor only watches peers in STATIC config (`state.rs:1246-1258`) and its pre-check
already refuses to adopt a shard a live third party owns (reads the durable shard-owner
directory, `cluster.rs` `read_shard_owner`, confirmed the `PeerLiveness` trait). But the
moment discovery feeds dynamic peers into the watch set, this decoupling breaks unless:

**The non-negotiable composition rule (discovery.md §5.3):**
- **FAILOVER** subject = a *previously-Active member of the durable `cluster/members`
  set* whose link is confirmed-down past debounce → adopt its shards (existing
  machinery, keyed on the MEMBER set).
- **MERGE** subject = a *previously-unknown cluster identity* (a synced replica carrying
  a different genesis / a `cluster/members` record this node is not in) → run cluster-
  union reconciliation, which must (a) treat NEITHER side's live owner as dead, and (b)
  surface same-key divergence loudly, never max-stamp-drop it.
- **Discovery feeds SEED addresses only; it must NEVER feed the failover watch set
  directly.** The supervisor consults the durable `cluster/members` set + shard-owner
  directory to decide adoption — exactly as it reads the durable directory today. This
  keeps sensor (discovery/beamr liveness) and authority (durable membership) separated —
  the same invariant discovery is built under, and the same
  haematite-as-source-of-truth / beamr-as-sensor split in the auto-memory.

**Flap guard:** a laptop mesh feeds `peer_connected=false` transients; the existing
debounce (`confirmations` consecutive down) is the guard, but a discovered peer that
co-formed, briefly linked, then dropped must NOT trigger an adopt of shards it never
owned in this node's durable view — which the member-set-keyed rule above enforces.

### 5.3 Zero-config footgun: SyncNodeId MUST be globally unique

All ballot uniqueness (and thus the whole fence + merge determinism) rests on
`SyncNodeId` being globally unique (epoch.md §2). Zero-config must derive it from
hostname + random, NOT a fixed default. If two nodes share a `SyncNodeId`, two clusters
CAN produce the same `(epoch, seq)` for one key with different bytes → `DuplicateStamp`
hard error at merge, node stays not-live. This is a concrete config-generation gate.

---

## 6. Gating correctness proofs / spikes (must pass BEFORE any build)

Each is a blocking negative-control. No implementation of the dependent phase begins
until its gate is green. Named to be run as harnessed tests.

- **GATE-ONE-CLUSTER (two-fresh-nodes-form-exactly-one).** Two fresh nodes, same `cn`,
  co-boot within the settling window on a healthy in-process mesh → assert EXACTLY ONE
  `cluster/members` record survives naming BOTH nodes, exactly one owner per shard, and
  the denominator equals 2. Run the min-election under artificially skewed candidate-set
  visibility (each node sees the other late) to exercise the co-form race. **Blocks
  Phase 3 (co-form).**

- **GATE-DIVERGENCE-DETECTED (same-workflow-divergence-is-detected-and-resolved).**
  Two nodes each genesis independently; each accepts a DIFFERENT event #N to the SAME
  `workflow_id` under its own owner/epoch; then merge. Assert: (i) the divergence
  detector FIRES at fork seq N (does NOT silently max-pick); (ii) the survivor run is
  the higher-epoch cluster's history VERBATIM (a real prefix, replay-clean, no splice);
  (iii) the loser's tail is quarantined under a new run-id with a `superseded` terminal
  event; (iv) NO `NonDeterminismError` on replay of the survivor. This is the honesty
  gate — today the merge would be a silent splice. **Blocks Phase 4 (data merge over
  E-streams) and Phase 5 (merge-on-discovery).**

- **GATE-CONVERGENCE (union is order-independent + surfaces the drop).** Two clusters,
  each writes the SAME `workflow_id`, merge in both orders → assert (i) identical merged
  root regardless of merge order (convergence — should PASS given existing proptests);
  (ii) the chosen survivor and the DROP are LOGGED/surfaced, not silent (competitive.md
  §6). **Blocks any use of `merge_committed_union` on cross-genesis roots.**

- **GATE-MERGE-SAFE-DENOM (SPIKE-FIRST).** Prove the denominator transition across a
  merge is safe. The spike must demonstrate the §3.2(a) reduction: a deterministic
  canonical-record election that does not race, then merge realised as a sequence of
  provably-safe single-change JOINs, with a negative control that a racing double-
  canonical cannot both win. Until this is green, merge-of-two-multi-node-clusters is
  NOT built (single-node-into-cluster JOIN is the only merge shipped). **Blocks Phase 5
  for the multi-node-both-sides case.**

- **GATE-NO-FAILOVER-CONFUSION.** Inject a newly-discovered same-`cn` peer while the
  failover supervisor runs → assert the supervisor does NOT adopt the newcomer's shards
  (newcomer not in durable member set), and that a MERGE trigger fires on a separate
  path treating neither live owner as dead. Flap variant: rapid connect/disconnect of a
  discovered peer must not trigger a spurious adopt. **Blocks Phase 5.**

- **GATE-SIDE-EFFECT-POLICY (SPIKE-FIRST, defers auto-resolution).** A run whose losing
  half fired a non-idempotent side effect, merged → assert the system does NOT
  auto-resolve/silently-drop; it quarantines and surfaces for operator compensation.
  This gate's PASS bar is "loud + quarantined," explicitly NOT "automatically correct."
  Automatic correct resolution of divergent runs remains OUT OF SCOPE until a dedicated
  spike closes the graph-shaped reconciliation + outbox reconciliation open questions.

---

## 7. Phased build decomposition (each landable + verifiable)

Dependencies flow downward; each phase ends at a green gate.

- **Phase 0 — SyncNodeId uniqueness + stamped-entry precondition.** Ensure zero-config
  derives a globally-unique `SyncNodeId` (hostname+random) and both merge preconditions
  hold (all entries stamped). Small, verifiable, unblocks everything. (§5.3)

- **Phase 1 — Serving gate (`FormationState`).** Mirror `ShutdownState`; bind early,
  admit late; gate mutations only (client AND internal shard-advance), never
  reads/replication/health/console. Retryable typed error while `Forming`. Verifiable in
  isolation with a fake formation signal. (§2.1) *No merge dependency — landable now.*

- **Phase 2 — #146 CSOT substrate.** Durable `cluster/members` record with persisted
  `cn`; per-write denominator reads the record (CSOT-1); single-node genesis write+read;
  JOIN-EXISTING via single-change +1 CAS. Single-node stays trivially correct. Gate: a
  node JOINs an already-formed cluster and the denominator updates by exactly +1 with
  overlapping majorities. (§5.1 items 2,5; membership.md Q1)

- **Phase 3 — Discovery + co-form (states A/B/C).** mDNS register/browse/filter by
  cn/pv; candidate accumulation; JOIN vs CO-FORM distinction via reading the peer's
  `cluster/members`; min(node_id) co-form election with a quiescence sub-window; the
  serving gate holds until FORMATION-SETTLED. **Gate: GATE-ONE-CLUSTER.** (§2, §3.2)

- **Phase 4 — Divergence detector + whole-run resolution policy.** aion-layer pre-merge
  pass over shared `E`-streams: clean prefix/extension → union; fork → quarantine +
  `superseded` terminal + re-parent; never splice. **Gates: GATE-DIVERGENCE-DETECTED,
  GATE-CONVERGENCE.** (§4)

- **Phase 5 — Merge-on-discovery orchestration.** The discovery-driven MERGE trigger;
  Axis-1 membership set-union resolver keyed on `cn`; Axis-2 forced per-shard
  re-election over union membership (reusing `acquire_shard`/`run_prepare_round`) with a
  per-shard quiesce gate; Axis-3 data union gated by Phase-4 detector; the failover-vs-
  merge separation. **Gates: GATE-MERGE-SAFE-DENOM (spike first), GATE-NO-FAILOVER-
  CONFUSION.** Multi-node-both-sides merge lands ONLY after GATE-MERGE-SAFE-DENOM is
  green; before that, only single-node→cluster JOIN and clean-prefix merges ship. (§3,
  §5.2)

- **Phase 6 (DEFERRED, spike) — automatic divergent-run resolution + side-effect
  reconciliation.** Graph-shaped fork reconciliation across children/timers/signals;
  outbox row reconciliation; automatic (not operator-driven) resolution. Gated on
  GATE-SIDE-EFFECT-POLICY graduating from "loud+quarantined" to "automatically correct."
  **Out of scope for the initial ship.** (§4.4, conflict.md §6)

---

## 8. Open decisions for Tom

1. **Pitch altitude.** All six analysts converge on: market this as "zero-config
   self-formation with automatic CONVERGENCE — two accidentally-split sub-clusters heal
   into ONE deterministic state instead of a permanent split," with an explicit bounded
   data-loss caveat on same-shard concurrent formation writes. Do you accept shipping
   under the *converge* framing (never claim *lossless merge*), which is genuine and a
   real cut above Serf/Akka/Cockroach? RECOMMEND yes.

2. **Fork policy: quarantine-and-alert vs auto-resolve.** The initial ship
   detects+quarantines a forked run (loud, fail-closed) and defers automatic correct
   resolution to a spike (Phase 6). Accept quarantine-only for v1? RECOMMEND yes — auto-
   resolution is graph-shaped and side-effect-entangled, a real research spike.

3. **Co-form (state B) at all, or JOIN/SELF-FORM only?** State B (deterministic
   min-election on a fresh mesh) is the ONLY multi-node-from-scratch path and it stays
   racy under mDNS eventual-consistency; its safety rides entirely on the gate +
   GATE-ONE-CLUSTER. The conservative alternative is to ship ONLY JOIN-EXISTING (state
   A) and SELF-FORM-ONE (state C), and require the FIRST node to be reached before
   others (still zero-flag: others simply find it). This removes the co-form race
   entirely at the cost of "first node must win the race to write genesis before a
   second boots." Decision fork: ship co-form (needs GATE-ONE-CLUSTER) vs defer co-form
   and lean on JOIN/self-form. RECOMMEND defer co-form initially; it is the riskiest
   branch and JOIN/self-form covers the realistic laptop-mesh flow (bring up one, others
   join).

4. **Off-LAN behaviour.** When mDNS is blocked (cloud VPC), discovery returns zero
   candidates → SELF-FORM-ONE, and a later-reachable peer is a lossy-risk merge. Accept
   the stated mitigation — require operator `seeds`/`peers` (already an override) or
   `--bootstrap` off-LAN, with ONE loud diagnostic line — i.e. "do not present
   zero-config auto-merge as safe off-LAN"? RECOMMEND yes.

5. **Settling-window default + shape.** Analysts recommend a 3 s default knob, with the
   gate holding for `max(window, first-peer-quiescence)` (also require the candidate set
   stable for a short sub-window). Accept 3 s default + quiescence requirement?
   RECOMMEND yes.

6. **Membership denominator transition path (§3.2).** Confirm the §3.2(a) approach
   (merge = deterministic canonical election + sequence of single-change JOINs) over
   §3.2(b) (new joint-consensus overlap proof). (a) is cheaper and reduces to the
   already-safe join path but needs a non-racing canonical election. RECOMMEND (a),
   gated by GATE-MERGE-SAFE-DENOM.

---

## 9. Grounding index (load-bearing source, re-confirmed for this doc)

- Union merge per-key max-stamp + `DuplicateStamp`:
  `haematite/crates/haematite/src/sync/handoff_merge.rs:121-176` (re-read 149-176).
- R-LE invariant (owner live_epoch strictly exceeds merged epochs; intra-cluster only):
  `haematite/crates/haematite/src/shard/actor.rs:1140-1200` (re-read `merge_adopt`).
- Stamp/Ballot total order, node tiebreak, `bottom`:
  `haematite/crates/haematite/src/sync_codec/ballot.rs:25-31,68-74`.
- Data fence + no-raise-on-accept: `shard/actor.rs:701-706,771-776`.
- Quorum denominator is per-write, `config.nodes.len()` today, self-quorum at 1:
  `haematite/crates/haematite/src/sync/membership.rs:29-75` (re-read),
  `consistency.rs:76-79,223-229,281-287`.
- Event key `stream_key || 0x00 || seq`, seq recovered from KEY:
  `haematite/crates/haematite/src/api/event_store.rs:11-54` (re-read).
- aion event stream key: `crates/aion-store-haematite/src/keyspace.rs:45-50` (re-read).
- `NotOwner` back-pressure (Fenced → NotOwner): `crates/aion-store-haematite/src/store.rs:1444-1447` (re-read).
- Replay single-history contract / NonDeterminismError / terminal-failure discipline:
  `crates/aion/src/durability/resolver.rs:101-145,184-195`, `determinism.rs:25-53`.
- Failover supervisor keys on member set + durable shard-owner directory (not discovery):
  `crates/aion-server/src/cluster.rs` (`PeerLiveness`: `peer_connected` + `read_shard_owner`, re-read).
- Serving surfaces bind-then-serve today; shutdown watch-flag pattern to mirror:
  `crates/aion-server/src/run.rs:161,188,221-222,275-279`, `shutdown.rs:64`.
- #146 substrate UNBUILT: `aion/docs/design/HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md`;
  discovery/split-genesis: `aion/docs/design/CLUSTER-AUTODISCOVERY.md`.

---

## Adversarial review verdict (2 independent critics, source-verified)

**SOUND-WITH-FIXES.** Every load-bearing claim was verified against haematite/aion `main` (the union
merge really is ancestor-free + history-independent + proptest-pinned; #146 is genuinely unbuilt with
its missing primitives named; the gates are real negative controls; Phase 1 is landable with no merge
dependency). Four fixes are REQUIRED before build:

- **B1 (soundness):** §4.3 must NOT justify survivor-selection with the forced-re-election epoch —
  that epoch governs FUTURE writes, not the ranking of the two PRE-merge histories (each carries its
  own genesis ballot, distinguished only by node-id). State the survivor tiebreak as
  arbitrary-but-deterministic (whole-run granularity + loud+quarantined), not "principled ordering."
- **B2 (liveness gate):** add GATE-MERGE-LIVENESS — a partial heal that cannot reach a majority of the
  UNION membership must leave the shard quiesced + surfaced, NEVER served under a minority.
- **B3 (side-effect control):** an orphaned LOCAL outbox row on the quarantined loser is a concrete
  dropped/double-fired side-effect path — make it an explicit negative control inside
  GATE-SIDE-EFFECT-POLICY.
- **B4 (co-form race):** GATE-ONE-CLUSTER must include an ASYMMETRIC-mDNS-visibility control (A sees
  {A,B}; B sees only {B}) asserting NO double genesis — the symmetric in-process mesh does not
  exercise the real race.
