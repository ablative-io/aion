# Design Brief — Runtime-Reload Server Endpoint (deploy API)

**Repo:** `/Users/tom/Developer/ablative/aion`, main @ `3a18cf5f` ("Bump all publishable crates and Gleam packages to 0.2.0"), clean tree.
**Parent brief:** [`runtime-package-reload.md`](runtime-package-reload.md) (#62). D3 adopted **(a)**: embedded-engine API now, serde-ready types, *"the aion-server endpoint is deferred to its own follow-up brief with a deploy-authz design."* This is that brief.
**Status:** decisions D1–D10 ADOPTED (Tom, 2026-06-12, recommendation (a) in every case); IMPLEMENTED — engine riders (typed refusals, `LoadOutcome`, manifest-digest tripwire), `DeployService` + `/deploy/*` over shared handlers, `deploy` claim / `x-aion-deploy` authz, `deploy_denied`/`version_pinned` wire codes, audit + metrics, `aion deploy/versions/route/unload`, docs.
**Scope:** expose the landed `Engine` reload seam (`load_package`, `route_workflow_version`, `list_workflow_versions`, `unload_workflow_version`) over the server's public transports as an **operator deploy API**, with a real authorization design. Plus the `aion-cli` remote subcommands that drive it. This brief is design-only; no implementation accompanies it.
**Why deploy is not a data operation:** loading a package registers code into the shared BEAM VM and re-points routing for a workflow *type*. In `SharedEngine` mode that type is startable from **every** namespace — there is no namespace→package binding anywhere in the engine (verified, §1.4). Namespace grants therefore authorize the wrong thing; deploy needs its own grant.

This brief is self-contained: an implementing agent needs no prior conversation context.

---

## 1. Verified current-state map

### 1.1 The landed engine seam (`crates/aion/src/engine/reload.rs`)

| Method | Behavior (verified) |
|---|---|
| `load_package(impl Into<WorkflowPackageSource>) -> Result<LoadedWorkflow, EngineError>` | Shutdown-gated; parse/validate → preflight → register → entry-verify → atomic route flip. Failure leaves the catalog bit-for-bit untouched. **Idempotent by content hash:** re-loading a loaded hash registers nothing but still re-points the route (re-deploy after rollback takes effect). |
| `list_workflow_versions() -> Result<Vec<WorkflowVersionInfo>, EngineError>` | Snapshot read, sorted `(type, loaded_at)`; `route_active` flag per version. This is the deploy read model. |
| `route_workflow_version(&str, &ContentHash)` | Atomic, idempotent re-point; typed refusal naming the loaded set for a never-loaded hash. |
| `unload_workflow_version(&str, &ContentHash)` | Swap-out-first then verify: refuses typed when route-active, when an in-flight start pins it, when a live registry handle pins it, or when a recoverable run / recorded-but-unstarted child in the store pins it (reload.rs:137-228). Catalog restored on refusal. |

`WorkflowVersionInfo` (`crates/aion/src/loader/version_info.rs`) is serde-ready per D3: `workflow_type`, `content_hash` (textual when serialized), `deployed_entry_module`, `entry_function`, `manifest_version`, `loaded_at`, `route_active`. `WorkflowPackageSource` is `Path(PathBuf) | Package(Box<Package>)` (`engine/builder.rs:37-42`) — a server holding archive **bytes** parses them via `aion_package::Package` and passes `Package`; no temp file needed.

**Gap A — refusals are stringly.** Every refusal above is `EngineError::Load { reason: String }` (`crates/aion/src/error.rs:24-28`). A wire adapter cannot map "version pinned by live run" vs "archive malformed" vs "unknown version" to distinct wire codes without parsing prose. The endpoint needs typed refusal variants first (§2.5, D5).

**Gap B — idempotent outcome is invisible.** `load_package` returns the same `LoadedWorkflow` whether it freshly registered, idempotently re-routed, or no-opped. A deploy response should say which happened; the engine must return it (§2.5).

**Gap C — same-hash-different-manifest is silently accepted.** Banked finding from #62, verified at `crates/aion-package/src/hash.rs:13-24` and `crates/aion/src/loader/catalog.rs:421-448`: the content hash covers the **canonical beam set only** — `manifest.json` (entry function, schemas, timeout, declared activities) is excluded. Two archives with identical beams but different manifests carry the **same** `ContentHash`; the catalog's idempotent path returns the existing record and the incoming manifest is silently ignored. Embedded callers control both archives; a *remote* deploy endpoint turns this into a silent wrong-deploy. Decision D10.

### 1.2 Server transport pattern

Two listeners (`main.rs:113-114, 153-165`): HTTP/axum on `server.listen_address` (workflow router + dashboard + `/metrics` + `/health/*`), gRPC/tonic on `server.grpc_address` hosting **both** `WorkflowService` and the worker-protocol service. Every operation exists on both transports through one shared handler layer (`api/handlers/`): the gRPC adapter and the axum routes each decode → build `CallerIdentity` → call `handlers::*` → encode; engine→wire error mapping lives in `handlers/error.rs` + `ServerError::to_wire_error` (`error.rs:114-131`). Drain: gRPC `start_workflow` refuses with `unavailable` while `drain_state().is_draining()` (`grpc.rs:49-53`).

### 1.3 Auth stack (three paths, verified)

1. **`auth.enabled = false` (dev mode):** caller built from `x-aion-subject` + `x-aion-namespaces` headers/metadata (`api/http/auth.rs:61-71`, `api/grpc.rs:295-307`). Wide open by design.
2. **`auth.enabled = true`, `auth` feature compiled (production):** `Authorization: Bearer <jwt>` validated against the JWKS cache. Claims today: `sub`, `namespace` (single string), `exp` (`auth/jwks.rs:69-74`); missing/invalid → redacted 401/`unauthenticated`.
3. **`auth.enabled = true`, no `auth` feature (dev-token):** `jwks_url` is treated as a static shared secret; subject/namespaces still come from the dev headers; each failure mode yields a specific `CallerIdentity::denied` reason (`api/http/auth.rs:78-119`).

`CallerIdentity` carries a `GrantSource` (`NamespacesHeader | TokenClaim`) so denial messages name the knob that actually carries the grant (`namespace/resolver.rs:28-34, 494-508`) — the deploy denial must follow this established pattern.

### 1.4 Namespace/engine reality (corrects the working assumption)

The working assumption was "NamespaceResolver supports shared-engine and per-namespace-engine modes." **Per-namespace engines do not exist in code.** `NamespaceMode` is `SharedEngine | SingleTenant { namespace }` (`config/mod.rs:203-212`); the resolver holds exactly one `Option<Arc<Engine>>` (`namespace/resolver.rs:280-285`) and `ScopedEngine` clones that one handle for every namespace. Per-namespace engines are a design-doc aspiration only (AW-006/AW-007 say the resolver *hides whether* the deployment uses them). Consequences:

- A package load is **engine-global**: after `load_package`, the workflow type is startable from every authorized namespace. There is no per-namespace catalog, no namespace column in `WorkflowVersionInfo`, no namespace→package binding to enforce.
- "Load into which engines?" has exactly one answer today. The wire surface should not pretend otherwise (D4); pre-1.0 NO-BACKWARDS-COMPATIBILITY means we extend the surface if/when a multi-engine mode lands, rather than shipping speculative targeting fields now.

### 1.5 Wire error taxonomy

`WireErrorCode` (`aion-proto/src/error.rs:39-78`): `NotFound`, `NamespaceDenied`, `SequenceConflict`, `UnknownQuery`, `QueryTimeout`, `NotRunning`, `Lagged`, `InvalidInput`, `Backend`, `QueryFailed`. Nothing fits "you may not deploy" (it is not a namespace denial) or "the version is pinned" (it is not not-found, not invalid input). Client SDKs branch on the stable string codes and fall back to their `Server`/unknown bucket for unrecognized codes, so **adding** rows is SDK-safe.

### 1.6 Package facts

- Archives are zips of beams + `manifest.json`; every example archive is **360–422 KB** (`examples/*/*.aion`). Even a large multi-module package is single-digit MB; tonic's default decode cap is 4 MB and axum's default body limit is 2 MB — adequate ceilings exist but must be an explicit operator decision per the house no-assumed-defaults rule (D3).
- `Package` construction recomputes the content hash from the beam set and verifies it against the manifest version, validates the entry module, and rejects engine-reserved module names (`aion-package/src/package.rs:70-90`, `error.rs`) — archive validation is inherent to load, not a separate pass.
- `PathEscapesRoot` is a **packaging-side** (`workflow.toml` project loader) error from #61; it cannot occur on the server load path, which never extracts to a filesystem root. The load-path validation taxonomy is `PackageError` (zip/manifest/entry/reserved-name) + catalog collision + entry-verification refusals.

### 1.7 CLI / client SDK current state

`aion-cli` (`crates/aion-cli/src/main.rs`) has the local-only `package` subcommand (explicitly "never connects to a server") and remote workflow operations driven through `aion-client` over gRPC with `--endpoint/--namespace/--subject` (dev headers; no bearer-token flag today, though `aion_client::ClientAuth::bearer` exists). `CLIENT-CONTRACT.md` scopes the caller SDKs to exactly `connect/start/signal/query/cancel/list/describe/subscribe` — deploy is outside the contract by construction.

---

## 2. Design

### 2.1 Config: an explicit `[deploy]` section

```toml
[deploy]
enabled = true                 # default false: the deploy surface is dark unless commissioned
max_archive_bytes = 16777216   # REQUIRED when enabled = true; no default (house rule)
```

- `enabled = false` (the default, consistent with `auth.enabled = false`): deploy routes are **not mounted** — HTTP 404, gRPC `Unimplemented`. A workflow server that is not a deploy target exposes no deploy attack surface at all.
- `max_archive_bytes` is the upload-size ceiling applied at both transports (axum `DefaultBodyLimit` on the deploy routes; explicit length check in the gRPC handler). Required-with-no-default follows the `query_timeout_ms`/`event_broadcast_capacity` pattern: validation fails loudly naming the key and the `AION_DEPLOY_MAX_ARCHIVE_BYTES` env override.

### 2.2 Authz: a `deploy` grant on `CallerIdentity` (D1)

- **JWT path:** `TokenClaims` gains `#[serde(default)] deploy: bool`. Absent claim = no grant; existing tokens keep working unchanged for data operations. `AuthenticatedClaims::caller_identity()` carries the grant.
- **Dev mode (`auth.enabled = false`):** grant from an `x-aion-deploy: true` header/metadata entry, mirroring `x-aion-namespaces`. Dev mode stays exactly as open as it already is for data ops, and the denial path remains testable.
- **Dev-token path:** shared-secret check as today, then the same `x-aion-deploy` header.
- `CallerIdentity` gains `deploy: bool` plus the existing `GrantSource`, and a `DeployGuard` (sibling of `NamespaceGuard`, in `namespace/` or a new `deploy/` module) makes the authorization decision **before any handler logic runs**. Denials are grant-source-aware, matching `namespace_denied` exactly:
  - header path: *"subject `ci` is not authorized to deploy; set x-aion-deploy: true for this caller"*
  - token path: *"subject `ci` is not authorized to deploy; mint a token whose deploy claim is true for subject `ci`"*
- The deploy grant is **deployment-wide**, not namespace-scoped, because the operation is engine-global (§1.4). A namespace-list-valued claim would promise an isolation the engine does not provide.

### 2.3 Wire surface (D2, D3, D4)

New proto file `crates/aion-proto/proto/deploy.proto`, separate service so `WorkflowService` (and therefore every caller SDK stub) is untouched:

```proto
service DeployService {
  rpc LoadPackage(LoadPackageRequest) returns (LoadPackageResponse);
  rpc ListVersions(ListVersionsRequest) returns (ListVersionsResponse);
  rpc RouteVersion(RouteVersionRequest) returns (RouteVersionResponse);
  rpc UnloadVersion(UnloadVersionRequest) returns (UnloadVersionResponse);
}

message LoadPackageRequest  { bytes archive = 1; }                       // a complete .aion archive
message LoadPackageResponse {
  string workflow_type = 1;
  string content_hash = 2;            // textual ContentHash — the version identifier everywhere on the wire
  string deployed_entry_module = 3;
  string entry_function = 4;
  bool   freshly_loaded = 5;          // false = idempotent re-load (hash already resident)
  bool   route_changed = 6;           // false = hash was already route-active (full no-op)
}
message ListVersionsRequest  {}                                          // engine-global; no namespace (D4)
message ListVersionsResponse { repeated WorkflowVersion versions = 1; }  // mirrors WorkflowVersionInfo incl. route_active, loaded_at
message RouteVersionRequest  { string workflow_type = 1; string content_hash = 2; }
message UnloadVersionRequest { string workflow_type = 1; string content_hash = 2; }
// RouteVersionResponse / UnloadVersionResponse are empty.
```

HTTP (mounted on the existing public router behind the same `DeployGuard`):

| Route | Body | Maps to |
|---|---|---|
| `POST /deploy/packages` | `application/octet-stream` raw archive bytes | `LoadPackage` (JSON response = `LoadPackageResponse` shape) |
| `GET /deploy/versions` | — | `ListVersions` |
| `POST /deploy/route` | JSON `{workflow_type, content_hash}` | `RouteVersion` |
| `POST /deploy/unload` | JSON `{workflow_type, content_hash}` | `UnloadVersion` |

- Both adapters call one shared handler module `api/handlers/deploy.rs` (the established `handlers/` pattern): authorize via `DeployGuard` → enforce `max_archive_bytes` → `Package::from_bytes` → `engine.load_package(...)` etc. gRPC `DeployService` joins the existing gRPC listener next to the worker protocol; no third listener.
- **Idempotency on the wire is specified behavior, not a decision:** re-POSTing the same archive succeeds with `freshly_loaded = false`, and `route_changed` reports whether the call re-pointed routing (the engine's re-deploy-after-rollback semantics surface verbatim). A deploy pipeline may retry blindly.
- **Drain/shutdown:** deploy mutations (`LoadPackage`, `RouteVersion`, `UnloadVersion`) refuse while `drain_state().is_draining()` exactly like `start_workflow`, and the engine's `ShutdownGate` covers the post-shutdown window. `ListVersions` keeps working during drain (read model for operators watching a rollout).

### 2.4 Error mapping (D5)

Two new `WireErrorCode` rows plus mappings:

| Condition | Engine source (post-rider, §2.5) | Wire code | HTTP | gRPC |
|---|---|---|---|---|
| Caller lacks deploy grant | `DeployGuard` denial | **`deploy_denied`** (new) | 403 | `PermissionDenied` |
| Unload/route refused: version pinned or route-active | `EngineError::VersionPinned` / `RouteActive` | **`version_pinned`** (new) | 409 | `FailedPrecondition` |
| Archive malformed / hash mismatch / reserved name / entry missing / collision / manifest-mismatch (D10) | `PackageError` + typed load refusals | `invalid_input` | 400 | `InvalidArgument` |
| Route/unload of an unknown `(type, hash)` | `EngineError::UnknownVersion` | `not_found` | 404 | `NotFound` |
| Archive exceeds `max_archive_bytes` | adapter check | `invalid_input` (message names the key and the limit) | 413 | `InvalidArgument` |
| Draining / shutting down | drain state / `EngineError::ShuttingDown` | `backend` with explicit message | 503 | `Unavailable` |
| Deploy surface disabled | not mounted | — | 404 | `Unimplemented` |

Refusal messages pass through the engine's prose (which names exactly what pins a version — run ids, child ids, in-flight starts); the wire code carries the branchable class.

### 2.5 Engine riders (small, prerequisite)

1. **Typed refusals (closes Gap A):** split deploy-relevant refusals out of `EngineError::Load { reason: String }` into structured variants — at minimum `UnknownVersion { workflow_type, version, loaded }`, `VersionPinned { workflow_type, version, pinned_by: PinHolder }` (in-flight start / live run / recoverable run / recorded child), `RouteActive { workflow_type, version }`, keeping `Load` for archive/registration failures. NO BACKWARDS COMPATIBILITY: replace the variants, fix the (engine-internal) construction sites and tests.
2. **Load outcome (closes Gap B):** `load_package` returns `LoadOutcome { record: LoadedWorkflow, freshly_loaded: bool, route_changed: bool }` computed inside the catalog's mutation lock (race-free truth, unlike a list-before/list-after read).
3. **Manifest-mismatch tripwire (closes Gap C, per D10):** the catalog retains a canonical manifest digest per loaded version; the idempotent re-load path compares the incoming package's digest and refuses typed (`invalid_input` on the wire) on mismatch instead of silently ignoring the new manifest.

### 2.6 Observability (D7)

- **Audit log:** one structured `tracing::info!` per deploy mutation with `operation` (`deploy.load` / `deploy.route` / `deploy.unload`), `subject`, `grant_source`, `transport`, `workflow_type`, `content_hash`, `outcome` (`loaded` / `idempotent` / `rerouted` / refusal class), and for loads `freshly_loaded`/`route_changed`. Denials log at `warn` with subject + grant source. This rides the existing log pipeline; who-did-what is answerable from logs alone.
- **Metrics** (existing `observability::Metrics` registry): counters `aion_deploy_operations_total{operation, outcome}` and `aion_deploy_denied_total{transport}`; gauge `aion_loaded_workflow_versions{workflow_type}` updated from the post-operation listing.
- **Read model:** `ListVersions`/`GET /deploy/versions` is the canonical "what is deployed and routed right now"; per-run forensics ("what version did run X execute") already live in durable history via the required `package_version` event field (#62 Wave 0).

### 2.7 CLI (D8)

New remote subcommands in `aion-cli` (the local `package` subcommand is untouched and stays offline):

| Command | Endpoint |
|---|---|
| `aion deploy <archive.aion>` | `LoadPackage`; prints type, hash, `freshly_loaded`, `route_changed` |
| `aion versions [--workflow-type T]` | `ListVersions` (client-side type filter) |
| `aion route <workflow-type> <content-hash>` | `RouteVersion` (rollback / roll-forward) |
| `aion unload <workflow-type> <content-hash>` | `UnloadVersion` |

- The deploy stub is generated from `deploy.proto` and lives **in `aion-cli`**, not in `aion-client` — the caller SDK surface is contract-bound (§1.7) and must not grow operator operations.
- Token sourcing: global `--token` flag overriding `AION_TOKEN` env; when absent, dev mode applies and the CLI sends `x-aion-deploy: true` alongside the existing `x-aion-subject` metadata. (The same `--token`/env plumbing becomes available to the data commands for free, but wiring those is out of scope here.)

---

## 3. DECISION points for Tom

### D1 — Deploy authorization model

- **(a) `deploy` claim in the existing JWT + dev-header analog + config-gated mount (§2.1–2.2)** — one auth stack, one token mint, one JWKS cache; the grant travels with the signed credential; grant-source-aware denials extend the established pattern verbatim; dev/dev-token modes degrade consistently. The `[deploy] enabled` gate doubles as the "separate admin surface" without new listeners.
- **(b) Separate admin token class** (distinct audience/issuer or second JWKS) — stronger separation of mints, but a second validation path, second cache, second config surface, and the deployment still has to decide which listener accepts it; heavy for what a boolean claim expresses.
- **(c) mTLS-gated admin listener** — strongest network-level isolation, but introduces a third listener and client-cert distribution for a pre-1.0 system whose TLS config is barely exercised; can be layered later without changing the claim model.
- **(d) Config-allowlisted subjects** — authorization data outlives tokens and lives in server config; rotation means config redeploys; no cryptographic binding between subject and grant in dev paths.

**Recommendation: (a).** (c) remains available later as defense-in-depth; (d) is rejected as config-coupled identity.

### D2 — Wire surface shape

- **(a) Both transports via the shared `handlers/deploy.rs`, new separate `DeployService` proto + `/deploy/*` HTTP routes, mounted only when `[deploy].enabled` (§2.3)** — matches the server's both-transports-one-handler pattern; a separate service keeps `WorkflowService` and every generated SDK stub byte-identical; dark-by-default minimizes attack surface.
- **(b) New RPCs on `WorkflowService`** — regenerates every SDK stub for an API clients must never call; entangles operator and caller surfaces forever.
- **(c) HTTP-only (or gRPC-only)** — breaks the established parity pattern; CI deploys want gRPC, humans/curl want HTTP; the shared handler makes "both" nearly free.

**Recommendation: (a).**

### D3 — Archive upload transport

- **(a) Single message/request (`bytes` field; raw `octet-stream` HTTP body) with required explicit `[deploy].max_archive_bytes` (§2.1)** — real archives are 360–422 KB (§1.6); a unary call keeps handlers, retries, and idempotency trivial; the explicit cap follows the house no-assumed-defaults pattern and the operator sizes it for their packages.
- **(b) Client-streamed chunks (gRPC streaming + HTTP multipart/chunked assembly)** — necessary only when archives approach transport limits (~3 orders of magnitude away); costs a streaming protocol, partial-upload states, and reassembly buffering on both transports.

**Recommendation: (a).** Revisit only if `.aion` packages ever carry large embedded assets.

### D4 — Deploy scope vs namespaces / multi-engine

- **(a) Engine-global operations, no namespace field anywhere on the deploy wire surface (§1.4, §2.3)** — tells the truth: one engine, namespace-blind catalog, a loaded type is startable from every namespace. `SingleTenant` deployments behave identically. If a per-namespace-engine mode ever lands, the deploy surface gains an explicit engine/namespace selector **then** (pre-1.0 breakage is permitted and honest).
- **(b) Namespace-scoped requests now** (require + record a namespace per load) — manufactures an isolation the engine does not enforce; a "tenant-a deploy" would still be startable from tenant-b, which is worse than no claim of isolation.
- **(c) Block the endpoint in `SharedEngine` multi-tenant deployments until namespace-bound packages exist** — punishes the only deployments that exist today; the deploy grant (D1) already restricts *who*, which is the actual risk.

**Recommendation: (a)**, with the engine-global blast radius stated plainly in the endpoint docs.

### D5 — Wire error codes + typed engine refusals

- **(a) Add `deploy_denied` and `version_pinned` to `WireErrorCode`; map the rest onto existing codes (§2.4); engine rider replaces stringly refusals with typed variants (§2.5.1)** — denial-vs-precondition-vs-validation become branchable by machines (CI gates on `version_pinned` to know a rollback target is still pinned); SDKs are unaffected (unknown codes already fall back; deploy codes never reach caller SDK paths anyway).
- **(b) Reuse `namespace_denied` for deploy denials and `invalid_input` for pin refusals** — no enum churn, but `namespace_denied` is defined as "no grant for the requested namespace" (there is no namespace here) and pin refusals are state conflicts, not malformed requests; both misclassifications poison the taxonomy.
- **(c) String-match the engine's refusal prose in the adapter** — forbidden by taste and by every future reword.

**Recommendation: (a).**

### D6 — Validate-only / dry-run load mode

- **(a) No dry-run in v1** — archive integrity is already proven locally at packaging time (`aion package` builds it; `Package` parsing re-proves it at load); load is failure-atomic (catalog untouched on any failure) and idempotent, so a failed or repeated real deploy is harmless; collisions are the only thing a dry-run would add and they are rare and loudly typed.
- **(b) `validate_only` flag** performing parse + preflight without registering — needs a new engine seam to preflight under the catalog lock without mutating, for a check whose answer can be stale by the time the real load runs.

**Recommendation: (a).** Add later as a pure extension if a deploy pipeline demonstrates the need.

### D7 — Observability / audit depth

- **(a) Structured tracing audit line per mutation + denial warns + the three metrics + listing as read model (§2.6)** — full who-did-what-from-where with zero new storage; run-level forensics already durable via `package_version` in history.
- **(b) Durable audit trail (deploy events in the event store or a server-side audit table)** — deploy operations are engine-level, not workflow-scoped; the event store is per-workflow by construction, so this means a new storage surface and schema for data the platform's log pipeline already captures. Premature.

**Recommendation: (a).** If a compliance requirement for tamper-evident deploy history materializes, design it as its own surface.

### D8 — CLI shape and token sourcing

- **(a) Four flat subcommands (`deploy` / `versions` / `route` / `unload`), deploy stub generated in `aion-cli` directly, `--token` / `AION_TOKEN` sourcing, dev-header fallback (§2.7)** — remote-only by construction (packaging stays offline); `aion-client` stays contract-pure.
- **(b) Deploy methods on `aion-client`** — leaks operator API into the caller SDK and, by precedent, into the Python/TypeScript SDK expectations; explicitly rejected by the CLIENT-CONTRACT scope.
- **(c) A separate `aion-admin` binary** — a second binary to install and version for four subcommands; the grant model (not the binary boundary) is the security line.

**Recommendation: (a).**

### D9 — Contract-docs impact

- **(a) CLIENT-CONTRACT.md: NO change** — this is an operator API, not a caller SDK operation; the contract's operation catalogue is closed (`connect/start/.../subscribe`) and deploy is deliberately outside it. Add one sentence to the contract's scope section stating that deploy/`DeployService` is operator surface that SDKs SHALL NOT expose, so the boundary is recorded rather than implicit. Conformance suite: unaffected (store-level). Worker protocol (`worker.proto`, worker SDKs): untouched — workers execute activities and are version-blind. Operator docs: the endpoint, config keys, authz model, and CLI commands are documented in `docs/API.md` + `docs/packaging.md`.
- **(b) Add deploy to CLIENT-CONTRACT as an optional SDK operation** — invites four SDK implementations of a privileged API nobody asked for.

**Recommendation: (a).**

### D10 — Same-hash-different-manifest collision (banked #62 finding)

- **(a) Engine rider: retain a canonical manifest digest per loaded version; typed refusal on idempotent re-load with a differing manifest (§2.5.3); content-hash format unchanged** — closes the silent wrong-deploy with a ~small, local change; the deploy response stays truthful (`freshly_loaded = false` only when the resident version is *actually* what was sent); recorded `package_version` values, deployed module names, and existing archives all keep their meaning.
- **(b) Make the content hash manifest-inclusive** — principled, but it changes the package format's version identity: every deployed name (`module$hash`), every recorded `package_version`, every built archive, and the #61 packaging determinism tests churn. That is a package-format brief, not a server-endpoint rider, and (a) removes the operational danger without it.
- **(c) Document the limitation only** — leaves a remote endpoint that can silently deploy the wrong entry function/schemas. Not acceptable for a deploy surface.

**Recommendation: (a)** now; (b) only if/when a package-format revision is scheduled for other reasons.

### D11 — Inflate ceiling for uploaded archives (post-review security rider)

Security review of the landed endpoint found that `max_archive_bytes` caps only the *compressed* upload: ZIP entries declare their own sizes, and a DEFLATE bomb under the upload ceiling inflates ~1000:1 in `Package::load_from_bytes` before the content-hash check — an OOM vector on the deploy surface. **Decision:**

- A second REQUIRED `[deploy]` key, `max_inflated_bytes` (env `AION_DEPLOY_MAX_INFLATED_BYTES`), mirrors `max_archive_bytes`' validation exactly: required when `enabled = true`, startup fails naming the key, zero rejected. Additionally `max_inflated_bytes < max_archive_bytes` is rejected at startup — an inflate ceiling below the upload ceiling would refuse archives the upload ceiling admits even stored uncompressed. Both ceilings must also fit `usize` on the host platform (32-bit targets), checked at startup.
- Enforcement lives in `aion-package`: every loading entry point takes an explicit `ExtractionLimits` (`bounded(max_inflated_bytes)` / `unbounded()`, no `Default`), and extraction charges a *running* total of inflated bytes across all entries — manifest included — through a `Read::take`-bounded reader, refusing with the typed `PackageError::InflatedSizeExceeded { limit }` the moment the budget would be passed. No truncation; single entries cannot individually buffer past the remaining budget.
- The deploy upload path maps the refusal onto the existing oversized-archive class: HTTP `413` / gRPC `InvalidArgument`, message naming `deploy.max_inflated_bytes`, recorded as a refused load in the audit line and `aion_deploy_operations_total` like every other adapter-level refusal. No new wire code.
- Trusted operator-local loads (engine startup `workflow_packages`, packaging tooling re-reading archives it just wrote, test fixtures) pass `ExtractionLimits::unbounded()` explicitly; `unbounded()`'s docs forbid it for network input.

---

## 4. Wire/contract impact summary

- **New:** `crates/aion-proto/proto/deploy.proto` (+ build inclusion, generated module, `WireEnvelope` not needed — deploy messages are flat), two `WireErrorCode` rows (`deploy_denied`, `version_pinned`), `[deploy]` config section + env overrides, `x-aion-deploy` dev header, `deploy` JWT claim, `/deploy/*` HTTP routes, `DeployService` on the existing gRPC listener.
- **Changed (engine riders):** `EngineError` refusal variants (typed), `load_package` return type (`LoadOutcome`), catalog retains a manifest digest. All engine-internal or embedded-API surface; Meridian's embedded use of the seam gains the same typed refusals (improvement, coordinated breakage is pre-1.0-sanctioned).
- **Unchanged:** `WorkflowService`, worker protocol, caller SDKs, CLIENT-CONTRACT operation catalogue (one scope sentence added), conformance suite, dashboard (may later consume `GET /deploy/versions`; out of scope).

## 5. Test plan

Handler/unit:

1. `DeployGuard`: granted vs denied per auth path (dev header, JWT claim via minted test tokens, dev-token), denial messages grant-source-aware (assert both hints, mirroring `denial_hint_names_the_grant_source`).
2. Error mapping table (§2.4) — every typed engine refusal → exact wire code/HTTP status/gRPC code; refusal prose passes through.
3. Config validation: `enabled = true` without `max_archive_bytes` fails startup naming key + env override; disabled surface returns 404/`Unimplemented` on every deploy route.

Server e2e (both transports, real engine, two compiled fixture versions from the #62 fixture set):

4. Deploy v1 → start → output v1; deploy v2 → `freshly_loaded = true`, `route_changed = true` → new starts v2; parked v1 instance completes v1 (the #62 semantics observed through the wire).
5. Idempotent re-deploy: same archive → 200, `freshly_loaded = false`, `route_changed = false`; after `route` back to v1, re-deploying v2 → `route_changed = true`.
6. `versions` listing shows both versions with correct `route_active` before/after `route`; `route` to unknown hash → `not_found`.
7. `unload` refusals over the wire: route-active → `version_pinned` 409; live-run pin → `version_pinned` naming the run; success path → version gone from listing, re-`route` to it → `not_found`.
8. Oversized archive → 413/`InvalidArgument` naming `deploy.max_archive_bytes`; malformed zip and same-hash-different-manifest archive (rebuild fixture manifest with a different entry function) → `invalid_input` with the manifest-mismatch message (D10 rider proof).
9. Unauthorized deploy attempts never reach the engine (assert via a counting/seamed engine or log-free engine state), and succeed-after-grant.
10. Drain: mutations refuse during drain; `versions` still serves; load racing server shutdown → 503/`Unavailable`, engine closes cleanly.
11. Audit/metrics: one structured audit line per mutation with subject/hash/outcome; counters increment per outcome class.

CLI:

12. `aion deploy/versions/route/unload` against a live server (dev mode + token mode): exit codes, JSON output shapes, `AION_TOKEN` vs `--token` precedence, actionable rendering of `deploy_denied` and `version_pinned`.

Gates: `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, full suite green, conformance green on both stores (must be untouched).

## 6. Waves

- **Wave 0 — engine riders (1 agent):** typed refusal variants, `LoadOutcome`, catalog manifest digest + mismatch refusal; fix construction sites/tests. *Exit: workspace green; new typed-refusal unit tests; mismatch e2e at the engine level.* (~400 LoC)
- **Wave 1 — proto + config + authz (1 agent):** `deploy.proto`, wire-code rows, `[deploy]` config + validation + env, `deploy` claim through all three auth paths, `CallerIdentity`/`DeployGuard` + denial messages. *Exit: tests 1–3.* (~600 LoC)
- **Wave 2 — handlers + transports (1 agent):** `handlers/deploy.rs`, gRPC service, HTTP routes, drain/size enforcement, audit + metrics, server e2e. *Exit: tests 4–11.* (~800 LoC)
- **Wave 3 — CLI + docs (1 agent, parallel with Wave 2 review):** subcommands, token sourcing, `docs/API.md`/`packaging.md`, CLIENT-CONTRACT scope sentence. *Exit: test 12.* (~400 LoC)
- **Wave 4 — review:** Fable-level rigorous review per CLAUDE.md (brief + intent + files); patient-records standard.

---

### Appendix: key file:line index (HEAD `3a18cf5f`)

| Concern | Location |
|---|---|
| Landed reload seam | `crates/aion/src/engine/reload.rs` (whole file) |
| `WorkflowVersionInfo` | `crates/aion/src/loader/version_info.rs:8-23` |
| Catalog idempotent path (silent manifest ignore) | `crates/aion/src/loader/catalog.rs:421-448` |
| `WorkflowPackageSource` / `package_from_source` | `crates/aion/src/engine/builder.rs:37-42` |
| Stringly `EngineError::Load` / `ShuttingDown` / `Runtime` | `crates/aion/src/error.rs:12-70` |
| Content hash = beams only | `crates/aion-package/src/hash.rs:13-24, 62-70` |
| Package integrity on parse | `crates/aion-package/src/package.rs:70-90` |
| `PackageError` taxonomy | `crates/aion-package/src/error.rs` |
| Transport mounting (two listeners; worker shares gRPC) | `crates/aion-server/src/main.rs:113-114, 153-165` |
| Shared handler pattern + error mapping | `crates/aion-server/src/api/handlers/mod.rs`, `handlers/error.rs`, `crates/aion-server/src/error.rs:114-131` |
| HTTP router | `crates/aion-server/src/api/http/router.rs:50-71` |
| JWT claims / JWKS | `crates/aion-server/src/auth/jwks.rs:69-74, 114-140` |
| Dev + dev-token caller paths | `crates/aion-server/src/api/http/auth.rs:29-119`, `api/grpc.rs:261-330` |
| `CallerIdentity` + `GrantSource` + denial hints | `crates/aion-server/src/namespace/resolver.rs:28-98, 494-508` |
| `NamespaceMode` (no per-namespace engines) | `crates/aion-server/src/config/mod.rs:203-212`, `namespace/resolver.rs:280-285` |
| `WireErrorCode` | `crates/aion-proto/src/error.rs:39-78` |
| Drain refusal precedent | `crates/aion-server/src/api/grpc.rs:49-53` |
| Required-config precedent | `crates/aion-server/src/config/mod.rs:237-241, 419-426` |
| CLI structure / local-only packaging | `crates/aion-cli/src/main.rs:51-73` |
| Caller-SDK contract scope | `docs/design/aion-clients/CLIENT-CONTRACT.md` (scope + operation catalogue) |
| Example archive sizes (360–422 KB) | `examples/*/*.aion` |
