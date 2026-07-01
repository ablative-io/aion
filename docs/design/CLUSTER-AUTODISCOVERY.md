# Zero-config cluster auto-discovery — mDNS-first seeding for a laptop mesh (DESIGN)

> Status: **design pass, read-only analysis. No production code changed by this doc.**
> 2026-07-01. Task #147. Written in the lineage of the other design passes
> ([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md) — #146,
> the direct parent), [NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md](../NAMESPACE-TASKQUEUE-SPLIT-DESIGN.md),
> [NODE-AFFINITY-DESIGN.md](../NODE-AFFINITY-DESIGN.md), the epoch-fence / ADR-021
> clean-partial fence work in `crates/aion/src/engine/fence.rs`).
>
> Folds into **#116** (fan-out / affinity maturity). **Depends on #146**: this doc
> targets the `SeedProvider` seam #146 defines
> ([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:348-364](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L348))
> and does NOT re-derive the membership model — it only fills the discovery slot.
>
> Sibling designs this builds ON (do not re-derive them):
> - **#146 HAEMATITE-CLUSTER-SOURCE-OF-TRUTH** — the durable `cluster/members`
>   authority, the bootstrap-seed model, the `SeedProvider` seam. This doc is the
>   `SeedProvider` implementation design; #146 is the socket it plugs into.

## TL;DR (read this first)

1. **The story.** Download one static binary, run it on three laptops on the same
   LAN, watch them find each other and form ONE cluster — no config files, no seed
   IPs typed by hand, no daemon to install. That is the ops-burden win over
   Temporal/Restate. mDNS is the first cut because it is the only mechanism that
   needs zero infrastructure on a bare LAN.

2. **Discovery is NOT a source of truth.** mDNS announces only **WHO + WHERE + a
   compatibility gate**. It feeds candidate dial addresses into the #146
   `SeedProvider` seam and STOPS. It never writes membership, never sizes a quorum,
   never evicts. The durable `cluster/members` record
   ([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:167-185](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L167))
   remains the only authority; mDNS only automates *finding the first peer to dial*.

3. **The cookie stays the gate.** mDNS is unauthenticated — anyone on the LAN can
   advertise `_aion._tcp.local.`. The design is sound ONLY because the real
   admission gate is beamr's MD5 cookie challenge handshake
   ([handshake.rs:527-529](../../../beamr/crates/beamr/src/distribution/handshake.rs#L527),
   reject at [handshake.rs:1016](../../../beamr/crates/beamr/src/distribution/handshake.rs#L1016))
   and membership is quorum-durable. **The cookie is NEVER advertised.** A
   discovered candidate that lacks the cookie simply fails the handshake.

4. **Crate: `mdns-sd` (pure-Rust, no daemon).** It is the only realistic option
   that keeps the "one static binary, no Avahi/Bonjour running" promise. Bindings
   crates (`zeroconf`, `astro-dnssd`) need an OS mDNS daemon and break headless
   Linux — exactly the dependency "inbreeding" the project rejects
   ([DURABLE-AGENTS-AS-INFRASTRUCTURE.md:34-42](../DURABLE-AGENTS-AS-INFRASTRUCTURE.md#L34)).

5. **The crux is bootstrap-seed, and #147 makes it SHARPER, not easier.** mDNS
   automates finding the seed; it does NOT remove the chicken-and-egg
   ([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:309-316](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L309)).
   On a fresh mesh with no operator flag, two laptops can each mDNS-discover the
   other and BOTH write genesis, producing two disjoint denominator-1 clusters
   (SPLIT GENESIS). **The only safe resolution is `--bootstrap` on exactly one
   node.** The deterministic lowest-`node_id` election (§4.3) makes split genesis
   *rarer* over a settling window but does **not** close the race, and once it
   happens the split is **permanent** (two self-quorum records under one key, no
   merge protocol — §8.1). So the fully-automated "just run it and they merge"
   path is a **demo-only convenience, not a correctness guarantee** — corrected here
   per adversarial review, which found an earlier draft over-claimed it as
   self-healing. Making the zero-flag path genuinely safe needs a distributed
   pre-write agreement that does not yet exist; it is UNBUILT and gated behind
   ADSC-2's mandatory race negative-control.

---

## 1. Goal + the zero-config pitch

### 1.1 The story we are buying

Temporal, Restate, Hatchet et al. make you stand up a control plane: a database,
a config file naming every node, a service mesh or a hand-maintained seed list.
The Ablative pitch is the opposite — a single self-contained binary
([DURABLE-AGENTS-AS-INFRASTRUCTURE.md:34-42](../DURABLE-AGENTS-AS-INFRASTRUCTURE.md#L34))
that forms a cluster with **zero operator input on a LAN**:

```
laptop-a$ aion server            # boots, announces itself, waits
laptop-b$ aion server            # discovers laptop-a, dials, joins
laptop-c$ aion server            # discovers a+b, dials, joins
# → one 3-node cluster, durable membership, no IPs typed
```

This is the visceral demo (cf. the Sydney failover demo memory): the operator
never edits TOML, never learns a peer IP, and the cluster is nonetheless a real
quorum-durable membership set the moment it forms.

### 1.2 Scope boundary — what mDNS is and is NOT

mDNS is the **first cut for the laptop-mesh story only**. It is best-effort,
LAN-only multicast: dropped by many managed switches, absent from most cloud/VPC
networks, and it does not cross subnets/VLANs. #146 already frames Tailscale and
SWIM-style gossip as *sibling* `SeedProvider`s for those environments
([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:355-359](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L355)).
**Do not oversell mDNS as general discovery.** This doc designs exactly one
provider behind exactly one trait; cloud discovery is a later, separate provider.

### 1.3 The non-negotiable invariant

Discovery MUST NOT become a second source of truth. Everything in this doc is
subordinate to that. The candidate stream mDNS produces is **advisory input** to
the #146 pipeline; the durable `cluster/members` record and the beamr cookie
handshake are the two real gates, and neither is weakened by adding discovery.

---

## 2. The mDNS mechanism — service type, TXT schema, crate

### 2.1 Crate recommendation: `mdns-sd`

Ranked against the project's no-inbreeding / single-binary posture
([DURABLE-AGENTS-AS-INFRASTRUCTURE.md:34-42](../DURABLE-AGENTS-AS-INFRASTRUCTURE.md#L34)):

| Crate | Kind | Advertise | Browse+resolve | Needs OS daemon | Verdict |
|---|---|---|---|---|---|
| **`mdns-sd`** | pure-Rust, speaks mDNS itself | ✅ | ✅ | **NO** | **RECOMMENDED** |
| `libmdns` | pure-Rust responder | ✅ | ❌ | no | reject — advertise-only, cannot browse |
| `zeroconf` | bindings → Avahi/Bonjour | ✅ | ✅ | **YES** | reject — daemon dep, breaks headless Linux |
| `astro-dnssd` | bindings → `dns_sd` | ✅ | ✅ | **YES** | reject — same daemon dep |

`mdns-sd` is the only option that keeps the promise "one static binary, no daemon,
works on a bare LAN," which is the whole point of #147. Bindings crates require the
host mDNS responder (Avahi on Linux, Bonjour on macOS) to be installed and running
— a headless Linux box without `avahi-daemon` has no responder, and the zero-config
story dies. `mdns-sd` carries its own multicast responder, so it needs nothing on
the host.

**Async caveat (glue, not hard):** `mdns-sd` is not natively tokio-async. It runs a
background service thread and exposes channel/poll receivers. It integrates via a
dedicated bridge task (or spawn_blocking receiver loop) feeding a tokio `mpsc`.
There is direct precedent in the codebase: `connect_haematite_store` already uses
`spawn_blocking` for the off-runtime endpoint bind
([state.rs:1176](../crates/aion-server/src/state.rs#L1176)), and beamr's
`AcceptHandle` has an explicit lifecycle to mirror on shutdown
([connection.rs:812](../../../beamr/crates/beamr/src/distribution/connection.rs#L812)).
The bridge must register on boot and **unregister on drain** so a graceful leave
withdraws the announcement.

### 2.2 Service type

Register under **`_aion._tcp.local.`** (DNS-SD service type, `.local.` mDNS domain).
One service type for the single cluster-replication role. The **instance name is the
`node_id`** (e.g. `node-0._aion._tcp.local.`) so instances are self-describing and
de-duplicate by identity — the same `node_id` that is the beamr distribution name
([connection.rs:388](../../../beamr/crates/beamr/src/distribution/connection.rs#L388))
and the membership identity
([sections.rs:115-117](../crates/aion-server/src/config/sections.rs#L115)).

### 2.3 TXT record schema

Advertise only WHO + WHERE + a compatibility gate. **Never the cookie/secret.**

| Key | Value | Maps to | Why |
|---|---|---|---|
| `nid` | `node_id` string | `ClusterPeer.name` / self-identity | the beamr distribution name / membership identity ([sections.rs:153](../crates/aion-server/src/config/sections.rs#L153)) |
| `repl` | replication `host:port` | `ClusterPeer.address` ([sections.rs:155](../crates/aion-server/src/config/sections.rs#L155)) | AUTHORITATIVE dial endpoint — explicit because a node may bind `0.0.0.0` but must advertise a routable addr (see §5.4) |
| `grpc` | optional gRPC `host:port` | `ClusterPeer.grpc_address` ([sections.rs:157-164](../crates/aion-server/src/config/sections.rs#L157)) | R-2/R-3 client-API forwarding; absent = not forwardable |
| `cn` | cluster name / cluster-id | scoping filter (§5.2) | **CRITICAL** for multi-tenant LAN safety: two unrelated clusters must NOT merge; a browsing node discards any record whose `cn` ≠ its own |
| `pv` | protocol/schema version (small int) | compatibility gate | a node ignores incompatible `pv` so a stray old binary cannot poison the seed list |

**Deliberately NOT advertised:**
- **The cookie / any secret.** The cookie is the real admission gate
  ([handshake.rs:527-529](../../../beamr/crates/beamr/src/distribution/handshake.rs#L527));
  putting it in TXT would expose it to the whole LAN.
- **`owned_shards`.** Placement is durable haematite state, learned from the store
  after dialing, not a discovery fact. A TXT `owned_shards` would be a stale second
  source of truth — precisely what §1.3 forbids.

### 2.4 Browse → resolve → candidate pipeline

1. **Register.** On boot, register this node's `_aion._tcp.local.` service with the
   §2.3 TXT (SRV/A carry host + `repl` port).
2. **Browse.** Concurrently browse `_aion._tcp.local.`; for each discovered
   instance, resolve to SRV + A + TXT.
3. **Filter.** Drop self (`nid` == own `node_id`), drop `cn` mismatch, drop `pv`
   incompatible.
4. **Map.** Each survivor → a candidate dial addr (`repl` → `SocketAddr`), yielding a
   de-duplicated candidate SEED list.
5. **Hand off.** Push that list to the #146 `SeedProvider` seam. **Discovery STOPS
   here.** The rest — dial, cookie handshake, sync, join — is the existing
   beamr-connect + `replicate_write` pipeline
   ([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:361-364](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L361)).

The provider runs continuously (a laptop mesh re-forms as nodes come and go),
coalescing/debouncing by `node_id` so it does not spam the membership layer with
candidate churn.

---

## 3. Seam into beamr distribution — discovered peers to dial

### 3.1 What the candidate must reconstruct

A resolved record must yield the shape of a `ClusterPeer`
([sections.rs:152-174](../crates/aion-server/src/config/sections.rs#L152)):
`name` (= `nid`), `address` (= `repl`), `grpc_address` (= `grpc`, optional),
`owned_shards` (LEFT EMPTY — learned from the store, never from TXT). This is
exactly the shape `connect_haematite_store` consumes today when it turns
`cluster.peers` into `watched_peers`
([state.rs:1140-1152](../crates/aion-server/src/state.rs#L1140)) and
`directory_peers` with the gRPC forward addr
([state.rs:1155-1168](../crates/aion-server/src/state.rs#L1155)), with
`self_node_id` from `cluster.node_id`
([state.rs:1171](../crates/aion-server/src/state.rs#L1171)), before
`build_haematite_store` maps them into the beamr-backed distributed store
([state.rs:1226-1275](../crates/aion-server/src/state.rs#L1226)).

### 3.2 The handshake is untouched

Discovery adds candidates to the *front* of the existing dial path; it does not
change how a dial is authenticated. A node binds its TCP listener via
`ConnectionManager::listen(listen_addr)`
([connection.rs:800-802](../../../beamr/crates/beamr/src/distribution/connection.rs#L800))
and identifies by `local_node_name`
([connection.rs:388](../../../beamr/crates/beamr/src/distribution/connection.rs#L388)).
A discovered candidate is dialed exactly like a statically-configured peer and must
pass the same MD5 cookie challenge
([handshake.rs:321-343,527-529](../../../beamr/crates/beamr/src/distribution/handshake.rs#L321)).
**A wrong/absent cookie is rejected**
([handshake.rs:1016](../../../beamr/crates/beamr/src/distribution/handshake.rs#L1016)),
which is exactly why an unauthenticated discovery layer is safe: the worst a
malicious LAN advertiser achieves is a failed handshake and a log line.

### 3.3 Integration shape — `MdnsSeedProvider` at the aion-server edge

Implement `MdnsSeedProvider` behind the #146 `SeedProvider` trait, owned at the
aion-server edge alongside `connect_haematite_store`
([state.rs:1127-1180](../crates/aion-server/src/state.rs#L1127)). Placing it here
(rather than lower in the crate chain) preserves the strict linear dependency chain
— the net-new `mdns-sd` dep lives at the top edge and does **not** diamond into the
beamr → haematite → liminal → aion chain
([DURABLE-AGENTS-AS-INFRASTRUCTURE.md:34-42](../DURABLE-AGENTS-AS-INFRASTRUCTURE.md#L34)).
The provider produces a fresh candidate stream; the membership layer decides what to
do with it under quorum.

---

## 4. Composition with the haematite membership authority (#146) + BOOTSTRAP-SEED

This is the crux section. It has two parts: how a discovered node *joins* an
existing cluster (easy, #146 already specifies it), and how a *fresh* mesh forms
its first cluster without splitting (the hard one).

### 4.1 The join flow — discovery feeds it, does not replace it

Once a cluster exists, a freshly discovered+connected node enters the authority via
the #146 JOIN flow
([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:227-233](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L227)):
boot with a seed → dial a seed peer (from mDNS) → **sync the store** → propose
`add N (status=Joining)` as a single-change CAS on `cluster/members` keyed by the
current epoch → a second delta promotes `Joining → Active` once its replica catches
up. The CAS is the verified `replicate_write` Strong-CAS-quorum-on-write primitive
([receiver.rs:84](../../../haematite/crates/haematite/src/db/receiver.rs#L84),
sequencing at [receiver.rs:49-63](../../../haematite/crates/haematite/src/db/receiver.rs#L49)),
sized against the CURRENT durable denominator — never against the mDNS-observed set.
Single-change (not joint consensus) is provably safe: any old-majority and any
new-majority differing by one element always overlap in ≥1 node.

**mDNS's entire contribution to join is step "dial a seed peer."** It does not
touch the denominator, the CAS, or the epoch. This is the narrow seam #146 defined
for exactly this purpose
([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:348-364](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L348)).

### 4.2 The BOOTSTRAP-SEED chicken-and-egg — stated plainly

You cannot reach quorum to WRITE the first `cluster/members` record until you can
already reach a quorum, and you cannot reach a quorum until you know SOME peer to
dial ([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:309-316](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L309),
[:44-48](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L44)). This is intrinsic to consensus
(Raft/etcd/Consul all keep a static seed list). **mDNS AUTOMATES finding the seed;
it does not remove the seed.** #146 is explicit that this cannot be designed away.

### 4.3 THE RESOLUTION — deterministic genesis-writer election over the discovered set

**This is the crux and the explicit answer to the crux.** On a fresh mesh, mDNS
gives every genesis node a *candidate set* but no authority to break the tie. If
each node independently decided "I'll write genesis," two laptops would each write a
denominator-1 genesis record and form two disjoint clusters (SPLIT GENESIS, §8).
The resolution has two paths, in precedence order:

1. **Explicit `--bootstrap` (human override, unambiguous).** When an operator wants
   a fresh cluster deliberately, they pass `--bootstrap` on exactly one node. That
   node writes the genesis `cluster/members` record naming the genesis set; all
   others discover it and JOIN (§4.1). This is #146's leaning recommendation
   ([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:405-408](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L405)).

2. **Deterministic lowest-`node_id` election (the zero-config fallback — the
   load-bearing piece).** For the fully-automated "three laptops, no flags" story,
   the genesis writer is elected **deterministically from the discovered candidate
   set plus self**: after a bounded discovery settling window, each node computes
   `min(node_id)` over `{self} ∪ discovered candidates (cn-matched, pv-matched)`.
   **Only the node whose own `node_id` equals that minimum writes genesis.** Every
   other node waits, observes the genesis record appear, and JOINs. Because
   `node_id` is a globally-unique string
   ([sections.rs:115-117](../crates/aion-server/src/config/sections.rs#L115)), the
   `min` is total and identical on every node **that has seen the same candidate
   set** — which is the eventual-consistency caveat that makes this the sharpest
   risk (§8).

**A local re-read before genesis reduces — but does NOT close — the race.** Before
writing genesis, a candidate writer re-reads `cluster/members` off its local replica;
if a record already exists (a prior boot formed the cluster, or this node saw a
winner's record replicate in), it does NOT write genesis — it JOINs instead. This
catches the *sequential* case (someone already won and I can see it) and is a genuine
safeguard for reboots of an already-formed cluster. Until that record exists, a lone
bootstrapping node is a valid **cluster of one** (denominator 1, self-quorum), which
is already a first-class config
([sections.rs:108-124](../crates/aion-server/src/config/sections.rs#L108)).

> **⚠️ This re-read is NOT a proof of safety for the concurrent case, and an earlier
> draft of this doc overstated it — corrected here per adversarial review.** It is
> tempting to borrow the "durable record wins over static intent" precedent from
> `build_haematite_store` ([state.rs:1242-1245](../crates/aion-server/src/state.rs#L1242),
> which reuses an on-disk `config.json` rather than re-creating). **That precedent
> does not transfer.** It is a *local, single-disk, zero-concurrency* check ("does MY
> filesystem already have config?"). The genesis re-read is a *distributed* check over
> a possibly-empty local replica. On a fresh mesh every node's `cluster/members` is
> locally empty, so the re-read passes for **all** elected genesis writers at once and
> provides **zero** protection against a simultaneous double-write. The only real
> guards against split genesis are (a) `--bootstrap` on exactly one node, or (b) a
> genuine distributed pre-write agreement that does not yet exist (§8.1). The
> deterministic `min(node_id)` election shrinks the window; it does not close it.

### 4.4 Settling window + stable node_id — the two things this election needs

- **Discovery settling window.** The deterministic election is only correct if every
  genesis node has discovered the same candidate set before computing `min`. mDNS is
  eventually consistent, so the fallback path MUST wait a bounded settling window
  (announce, then browse for T seconds, THEN elect) to shrink — not eliminate — the
  window in which two nodes see different sets. This does not make it provably
  split-free (§8); it makes split-genesis *rarer*, but — as corrected in §4.3 — it is
  NOT self-healing once two disjoint genesis records exist. The settling window is a
  probability reduction, not a correctness guarantee.
- **Stable `node_id` across IP change.** A laptop that changes IP (Wi-Fi → wired)
  must not be treated as a brand-new member. The `cluster/members` record keys on
  `node_id`, and the mDNS instance name IS the `node_id`, so a node re-announcing on
  a new `repl` addr updates its endpoint under the same identity rather than minting
  a new member. #146 does not address this
  ([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md open q](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L405));
  it is a #147-side concern resolved by making `node_id` the stable key and `repl`
  the mutable endpoint.

---

## 5. Security, cluster-scoping, and fallback

### 5.1 Security boundary — the cookie is the only gate that matters

mDNS is unauthenticated; anyone on the LAN can advertise `_aion._tcp.local.`. The
design is sound **only** because:
1. The beamr MD5 cookie challenge
   ([handshake.rs:527-529](../../../beamr/crates/beamr/src/distribution/handshake.rs#L527))
   rejects any dialer without the shared cookie
   ([handshake.rs:1016](../../../beamr/crates/beamr/src/distribution/handshake.rs#L1016)).
2. Membership is quorum-durable — a foreign node that somehow connects still cannot
   reach quorum in a cluster it was never added to.

**Two hard rules:** never let discovery bypass the cookie handshake, and never put
the cookie in a TXT record. mDNS announces WHERE + WHO; the cookie remains the lock.

### 5.2 Cluster scoping — the `cn` filter is load-bearing for multi-tenant LANs

Two unrelated aion clusters on one office LAN (two developers, two CI runners) must
NOT merge. Safety rests ENTIRELY on the `cn` (cluster name) TXT filter: a browsing
node discards any record whose `cn` ≠ its own configured cluster name. If `cn` is
omitted or mis-defaulted, both clusters try to merge; the durable membership
authority still prevents split-brain *writes* (a foreign node cannot reach quorum in
a cluster it was never added to), but you get connection churn and confusing logs.
**`cn` MUST be mandatory whenever discovery is enabled.**

**Recommended `cn` derivation (open, §8):** derive `cn` from a hash of the beamr
cookie. This ties the discovery scope to the auth domain — only same-cookie nodes
compute a matching `cn` — WITHOUT exposing the secret (a hash is not the cookie).
Two clusters with different cookies then have different `cn` and never even consider
merging, and the property is automatic rather than a field an operator can forget.

### 5.3 Fallback + precedence

- **Static config always wins as an explicit override.** If an operator supplies
  static `seeds`/`peers`, those are used directly; discovery is additive, never a
  replacement for an explicit list. Discovery fills the seed list when it is empty.
- **Multicast-blocked degradation.** On a network that drops multicast (managed
  switch, cloud VPC), mDNS simply returns no candidates. The node degrades to
  whatever static seed it has; with none it is a bootstrapping cluster of one. This
  is a **quiet, non-fatal** degradation — the binary still runs, it just does not
  auto-find peers. Emit a single clear log line ("mDNS discovery found no peers;
  supply `seeds` or `--bootstrap` for a non-multicast network") so the operator is
  not left guessing. This is where Tailscale/gossip providers (#146) take over.

### 5.4 The fiddly real-world bit — advertising a routable addr

`bind_address` may be `0.0.0.0` or an ephemeral port. The advertised `repl`/SRV MUST
carry a ROUTABLE address+port, which means resolving the actual bound addr (beamr
exposes `AcceptHandle::local_addr`,
[connection.rs:333](../../../beamr/crates/beamr/src/distribution/connection.rs#L333))
and picking a LAN-routable interface IP. This may need an `if-addrs`-style lookup
(NOT currently a dep) or an explicit `advertise_addr` config knob. This
interface-selection step is the fiddliest real-world part on multi-homed laptops
(§8).

---

## 6. UX, lifecycle, and ops-console surface

### 6.1 UX — zero flags is the target, one flag is the escape hatch

- **`aion server` with discovery on:** announces, browses, elects genesis or joins.
  No IPs, no member list. This is the demo.
- **`aion server --bootstrap`:** deliberately forms a fresh cluster from this node
  (the unambiguous human path, §4.3).
- **`aion server` with static `seeds`/`peers`:** discovery is bypassed/augmented;
  explicit config wins (§5.3). This is the cloud/CI path.

### 6.2 Lifecycle — register on boot, WITHDRAW on drain

The `mdns-sd` service must register on boot and **unregister on graceful drain**, so
a clean `LEAVE` withdraws the announcement and peers stop offering it as a seed. This
must compose with the server's `DrainConfig` shutdown and mirror the beamr
`AcceptHandle` teardown
([connection.rs:812](../../../beamr/crates/beamr/src/distribution/connection.rs#L812))
and the #146 graceful-LEAVE flow
([HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md:227-233](HAEMATITE-CLUSTER-SOURCE-OF-TRUTH.md#L227)).
A crash (no clean drain) leaves a stale announcement briefly; that is harmless — a
dialer just fails the connect and the candidate is dropped, and the durable
membership DEATH-debounce (#146) handles the actual eviction.

### 6.3 Ops-console surface — observe, do not author

Per the ops-console-out-of-box direction, the console should SHOW discovery as a
liveness/telemetry surface: discovered-but-not-yet-joined candidates, `cn`/`pv`
mismatches that were filtered (a great "why isn't my other laptop joining?"
diagnostic), and the elected genesis writer. It is **read-only**: the console must
NOT let an operator promote a discovered candidate into membership by hand —
membership changes go only through the quorum CAS. Discovery is a sensor surface,
consistent with §1.3.

---

## 7. Spike-first slice pipeline

Smallest-first, full clippy bar, no shims — mirroring the #146 CSOT plan and the
`engine/fence.rs` negative-control discipline
([fence.rs:129](../crates/aion/src/engine/fence.rs#L129),
[:369](../crates/aion/src/engine/fence.rs#L369)). **This pipeline depends on #146's
`SeedProvider` trait + `seeds` field existing first (CSOT-1).**

- **ADSC-0 (spike, no aion deps) — mDNS round-trip.** A standalone binary/test:
  register a `_aion._tcp.local.` service with the §2.3 TXT, browse from a second
  process, resolve, print the reconstructed `ClusterPeer` shape. Proves `mdns-sd`
  does advertise+browse+resolve with no host daemon on both macOS and headless
  Linux. **Falsifiable control:** assert a `cn`-mismatched and a `pv`-mismatched
  record are FILTERED OUT (the negative control that proves the filter works, same
  discipline as `plan_adopted_shards_prefix_buggy`).
- **ADSC-1 — `SeedProvider` impl.** Implement `MdnsSeedProvider` behind the #146
  trait at the aion-server edge (§3.3); produce a de-duplicated candidate stream
  into a tokio `mpsc`; register/unregister on boot/drain (§6.2). No genesis logic
  yet — feed candidates into the existing static path.
- **ADSC-2 — deterministic genesis-writer election.** The §4.3 fallback: settling
  window, `min(node_id)` over the candidate set, durable-precedence re-read before
  writing genesis. **Falsifiable control (THE key test):** a two-node fresh-mesh
  harness where both nodes discover each other must form exactly ONE cluster; a
  "both-write-genesis" buggy variant must produce two clusters and be DETECTED. This
  is the load-bearing test for the whole story.
- **ADSC-3 — `cn`-from-cookie + `advertise_addr`.** Wire `cn` derivation from the
  cookie hash (§5.2) and routable-addr selection / `advertise_addr` (§5.4). Prove
  two different-cookie clusters on one interface never merge.
- **ADSC-4 — lifecycle + degradation.** Drain withdraws the announcement; a
  multicast-blocked run degrades quietly with the diagnostic log (§5.3); IP-change
  re-announce keeps the same `node_id` member (§4.4).

Feature-gate `mdns-sd` behind a cargo feature (§8 open decision) so the spike does
not force the dep on the minimal binary before the on/off-by-default call is made.

---

## 8. Open decisions + honest risks

### 8.1 The biggest risk — SPLIT GENESIS on a fresh mesh

The "three laptops find each other and form ONE cluster" story hinges entirely on
the §4.3 deterministic genesis-writer election, and that election runs over an
**eventually-consistent** mDNS view. If two laptops each compute `min(node_id)` over
*different* candidate sets (because discovery has not settled), both can conclude
"I am the minimum" and both write genesis, producing two disjoint denominator-1
clusters. The settling window (§4.4) shrinks but does not eliminate this.

**And once it happens it is PERMANENT, not self-healing** (corrected per adversarial
review). Node A holds `cluster/members = {A}` (denominator 1, self-quorum) and node B
holds `cluster/members = {B}`, both durable and self-sufficient **under the same
key**. The only membership primitive #146 designs is a single-change JOIN that CASes
against *the other's* record — but each node already has its own authoritative record
under that key, and no merge protocol between two independently-genesised same-key
records exists. So this is an unrecoverable split-brain for the fully-automated path,
not a transient the loser recovers from. The local re-read of §4.3 does NOT save it
(both replicas are empty at genesis time; see the §4.3 correction box).

**Consequence for scope:** the deterministic zero-config genesis path is a
*best-effort demo convenience only*. For any cluster that must be correct,
`--bootstrap` on exactly one node is REQUIRED, not optional. **This is the sharpest
#147 risk, it is unbuilt, and it must be proven hard in ADSC-2 with a mandatory
negative control (two nodes racing genesis).** The real fix — if the automated path
is ever to be safe — is a genuine distributed pre-write agreement (a
"confirm-no-genesis-across-a-quorum-of-candidates before writing" round), which
itself needs a quorum that does not exist pre-genesis: the same chicken-and-egg
`--bootstrap` sidesteps. Until that is designed and proven, treat automated
fresh-mesh formation as unsafe beyond a single-writer demo.

### 8.2 Open decisions

1. **`cn` source.** New `[store.cluster].cluster_name` field vs derive from a hash of
   the beamr cookie. LEAN: derive from the cookie hash (§5.2) — ties scope to the
   auth domain automatically, cannot be forgotten, does not expose the secret.
2. **`pv` semantics.** Track the beamr handshake version, the haematite replication
   wire version, or an aion-discovery-schema version? LEAN: the last — an
   aion-discovery-schema version bumped ONLY when the TXT schema itself changes,
   independent of the wire protocols.
3. **Advertised-addr selection.** Add an `if-addrs`-style dep for automatic
   LAN-interface selection, or require an explicit `advertise_addr` when
   `bind_address` is `0.0.0.0`/ephemeral? LEAN: support `advertise_addr` first
   (deterministic, no new dep), add auto-selection later for the true zero-config
   multi-homed case.
4. **Genesis-writer path for automation.** Is deterministic lowest-`node_id` (§4.3)
   sufficient for zero-config, or should fully-automated fresh-mesh formation
   REQUIRE that a demo/CI harness pass `--bootstrap`? OPEN and load-bearing (§8.1).
5. **`SeedProvider` trait placement.** aion-server edge (preserves the linear dep
   chain, §3.3) vs a lower shared crate (if liminal ever needs discovery). LEAN:
   aion-server edge until a second consumer exists — do not diamond the chain
   speculatively.
6. **Feature-gating.** `discovery-mdns` off-by-default (lean binary) vs on-by-default
   (the zero-config/ops-console-is-the-default philosophy). Genuine tension: the
   zero-config promise argues on-by-default; the lean-single-binary posture argues
   gated. LEAN: on-by-default for the shipped binary, feature-gated so a truly
   minimal build can drop it.
7. **`mdns-sd` shutdown composition.** Confirm the background-thread model withdraws
   registrations cleanly under `DrainConfig` (§6.2). Verify in ADSC-4.

### 8.3 Honest risks (non-genesis)

- **mDNS is LAN-only and best-effort.** Useless in cloud/VPC (§1.2). Do not ship it
  as the general answer; it is the laptop-mesh cut, with Tailscale/gossip as the
  cloud siblings on the same seam.
- **Flap on a laptop mesh.** Nodes appear/leave constantly; the provider must
  de-dup by `node_id` and debounce or it spams the membership layer (§2.4). The
  #146 membership-side DEATH debounce is the real guard, but the provider should
  coalesce too.
- **Everything downstream of the seam is #146, and #146 is UNBUILT.** No
  `cluster/members` record, no `seeds` field, no `SeedProvider` trait, no
  `propose_membership_delta` exists in code yet (membership is still static config,
  `total_nodes = config.nodes.len()`,
  [membership.rs:58](../../../haematite/crates/haematite/src/sync/membership.rs#L58)).
  #147 cannot land before #146's CSOT-1 gives it a socket. Sequence: #146 CSOT-1 →
  #147 ADSC-1.
- **Stale routable-addr selection** on multi-homed laptops (§5.4) is a real,
  fiddly source of "it discovered the peer but can't dial it" bugs. `advertise_addr`
  is the safe first answer.

---

## Appendix — one-paragraph summary for the impatient

`mdns-sd` (pure-Rust, no daemon) advertises each node as
`<node_id>._aion._tcp.local.` with a TXT of `nid`/`repl`/`grpc`/`cn`/`pv` — WHO +
WHERE + a compatibility gate, **never the cookie**. Browsing nodes resolve, filter
by `cn`/`pv`/self, and hand a de-duplicated candidate dial list to the #146
`SeedProvider` seam; from there the existing beamr cookie handshake + `cluster/members`
quorum CAS take over unchanged. Discovery is a sensor, not an authority. The crux is
bootstrap-seed: mDNS finds the seed but cannot remove it, and on a fresh mesh a
deterministic lowest-`node_id` genesis-writer election (with `--bootstrap` as the
human override and durable-precedence re-read as the safety net) is what makes three
laptops form ONE cluster instead of two — that election runs over an
eventually-consistent view and is the single biggest, still-unbuilt risk.
