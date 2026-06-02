---
type: design
cluster: aion-dashboard
title: Aion Dashboard — Monitoring Web UI for the Standalone Server
---

# Aion Dashboard — Monitoring Web UI for the Standalone Server

> Part of the **Aion** durable workflow engine. See
> `docs/design/workflow-engine/DESIGN-OVERVIEW.md` for the whole-system
> vision and `COMPONENT-ARCHITECTURE.md` for the crate map.

## Intention

When an operator runs `aion-server` in production, they need to see what the
engine is doing: which workflows are running, which have failed, what
happened inside a specific execution, and what is happening *right now*. The
dashboard is that window. It is the human-facing operational surface of the
standalone server — the thing an on-call engineer opens at 3 a.m. when a
payment flow is stuck.

This cluster builds the **frontend web UI only**. It is a browser
application that consumes the HTTP/gRPC list/query/history API and the
WebSocket event stream that the server (cluster **AW**) exposes, and that the
server hosts as static assets. The dashboard does not run the engine, does
not persist anything, and does not define the API — it renders what the
server gives it, live.

It must feel inevitable to an operator who has used any workflow console:
a filterable list of workflows, a detail view that renders an execution's
event history as a readable timeline, and live updates that arrive without a
refresh. It must also feel honest under failure — when the WebSocket drops,
the operator sees it and the stream resumes; when a query fails, the error is
visible, not a blank screen; when there are no workflows, the empty state
says so rather than spinning forever.

When this cluster is done, an operator pointed at a running `aion-server` can
observe every workflow in every namespace, drill into any single execution's
full history, and watch events stream in real time — with production-grade
loading, empty, error, and reconnection behaviour throughout.

## Problem

A workflow engine without observability is a black box. Temporal's adoption
owes a great deal to its Web UI: operators trust what they can see. Aion's
server exposes the right primitives — a query/list API, a per-workflow
history API, and a first-class WebSocket event stream (the engine publishes
every appended event to a broadcast channel; the server fans it out filtered
or firehose). But primitives are not a product. Someone has to render them.

The rendering is not trivial:

- **The list must scale.** A production server holds thousands of workflows
  across namespaces. The list view must paginate against the server's query
  API and filter by type, status, and time range without loading every
  history.
- **History is the hard part.** A single workflow's value to an operator is
  its event history — the ordered story of activities scheduled and
  completed, timers started and fired, signals received, child workflows
  spawned, and the terminal outcome. Rendered as a flat JSON dump it is
  useless; rendered as a timeline keyed off the event envelope (sequence,
  timestamp, type) it tells the story at a glance.
- **Live is the differentiator.** Aion streams events as they happen. The
  detail view of a running workflow should grow as events arrive; the list
  should reflect status transitions live. This means a real WebSocket
  subscription model — per-workflow on the detail view, filtered/firehose on
  the list — with the reconnection and replay discipline that a long-lived
  operator session demands.
- **Production UIs fail gracefully.** Loading, empty, and error states for
  every async surface. A dropped socket must reconnect and resync, not
  silently stop updating. Namespace selection (the server's multi-tenancy)
  must scope every query and subscription.

This must match the **existing house frontend stack** rather than inventing a
new one. The repository already ships a sophisticated React app in
`apps/web` (React 19 + TypeScript + Vite + Tailwind v4 + Biome + Bun, shadcn/
Radix primitives, TanStack Query for server state, a singleton WebSocket
manager with reconnect, generated types from Rust). The dashboard reuses
those exact tools, conventions, and patterns so it reads as part of the same
codebase — feature folders with `components/`/`hooks/`/`types.ts`/`index.ts`,
React Query for fetches, a WebSocket manager for the live stream.

## Solution

A standalone Vite single-page application, **`apps/aion-dashboard`**, built on
the same stack as `apps/web`, that the `aion-server` binary serves as static
assets (cluster AW owns the hosting; this cluster owns the bundle). It talks
to the server over two channels: REST for list/query/history, WebSocket for
the live event stream.

The app is organised as feature folders matching house convention:

- **`api/`** — a typed client over the server's REST endpoints
  (`list`/`query`, `history`, `namespaces`) and a WebSocket manager modelled
  on `apps/web`'s singleton-with-reconnect, specialised for the Aion event
  protocol (subscribe by filter, receive `Event` frames, resync after a
  drop). Wire types (`Event`, `WorkflowSummary`, `WorkflowFilter`,
  `WorkflowStatus`, namespace) come from `aion-core`/`aion-proto` via the
  house generated-types pipeline — the dashboard never hand-defines a wire
  shape that the engine owns.

- **`features/workflow-list/`** — the paginated, filterable list. Filter
  controls (type, status, time range, namespace) drive a React Query against
  the query API; pagination is cursor- or page-based per the server's
  contract. A filtered WebSocket subscription updates rows live (status
  transitions, new workflows matching the filter). Full loading/empty/error
  states.

- **`features/workflow-detail/`** — the history viewer. Loads a single
  workflow's full event history from the history API and renders it as a
  vertical timeline: one entry per event, grouped/iconed by kind (activity,
  timer, signal, child workflow, lifecycle), showing the envelope (sequence,
  timestamp) and the decoded payload summary. A per-workflow WebSocket
  subscription appends new events live as the execution progresses, and
  reflects the terminal outcome (completed/failed/cancelled/timed-out).

- **`features/live-feed/`** — the real-time concerns shared by both views:
  the subscription lifecycle, the reconnect-and-resync logic, a connection
  indicator, and the firehose option for an unfiltered observability stream.

- **`features/namespace/`** — namespace selection (the server's
  multi-tenancy). The selected namespace scopes every query and every
  subscription; switching it re-scopes both.

- **`components/`** — shadcn/Radix primitives and dashboard-specific
  presentational pieces (status badge, event-type icon, timeline row, filter
  bar, connection indicator, empty/error panels) shared across features.

- **`app/`** — the shell: routing (`/` list, `/workflows/:id` detail),
  providers (QueryClient, namespace context, WebSocket), and layout.

### The Boundary With the Server (AW)

This is the load-bearing boundary of the cluster. **The dashboard consumes;
it does not provide.**

- The REST API surface — endpoints, request/response shapes, pagination
  contract, namespace parameter — is defined by **AW**. This cluster codes
  against that contract and treats it as fixed. Where AW's exact path or
  parameter name is not yet pinned, the dashboard's api module isolates it in
  one place (the client) so a contract adjustment is a one-file change.
- The WebSocket endpoint — its URL, the subscribe/unsubscribe message
  protocol, the event frame shape, and the firehose/filter semantics — is
  defined by **AW** (which in turn forwards the engine's `EventFilter`
  subscription model from AE/AT). The dashboard implements a client for that
  protocol; it does not design the protocol.
- Static-asset hosting (where the built bundle lives, how the server serves
  it, the base path) is **AW**. This cluster produces a standard Vite build
  output; how it is embedded or served is AW's concern. The build is
  configured to be servable under a base path so AW can mount it.

The wire types are the contract's source of truth. They originate in
`aion-core` (`Event`, `WorkflowSummary`, `WorkflowStatus`, `WorkflowFilter`)
and `aion-proto` (the gRPC/serde wire mapping), surfaced to TypeScript via the
house generated-types mechanism (Rust `#[derive(TS)]` → `src/types/
generated/`, never hand-edited — the same discipline `apps/web` follows).
The dashboard imports those generated types; it does not redefine them.

### The Event Timeline — the Core View

The history viewer is the cluster's centre of gravity. The `Event` enum from
`aion-core` is its data model. Every variant maps to a timeline entry:

- **Lifecycle** — `WorkflowStarted` opens the timeline; `WorkflowCompleted`/
  `WorkflowFailed`/`WorkflowCancelled` close it with the terminal outcome and
  result/error payload.
- **Activity** — `ActivityScheduled` → `ActivityStarted` → `ActivityCompleted`
  / `ActivityFailed` render as a correlated group keyed by `ActivityId`, so an
  operator sees an activity's full lifecycle (including retries via repeated
  `ActivityFailed` attempts) as one expandable unit rather than four
  disconnected rows.
- **Timer** — `TimerStarted` / `TimerFired` / `TimerCancelled`, correlated by
  `TimerId`, showing the scheduled duration and fire/cancel.
- **Signal** — `SignalReceived` with the signal name and payload summary.
- **Child workflow** — `ChildWorkflowStarted` / `ChildWorkflowCompleted` /
  `ChildWorkflowFailed`, with a link to the child's own detail view.

Each entry shows its envelope (sequence number for ordering, recorded
timestamp for when), an icon and colour for its kind, and a one-line summary
with the decoded payload expandable on demand. The timeline is strictly
ordered by sequence number — the same ordering the engine and store
guarantee — so live-appended events slot in correctly.

### Live Updates — Subscription and Reconnection

Both list and detail views are live. The model mirrors `apps/web`'s
WebSocket manager: a singleton connection with bounded reconnect, a set of
message handlers, and connect/disconnect notifications driving a connection
indicator. The Aion specialisation is the **subscription protocol**:

- The detail view subscribes to a single `workflow_id` filter; incoming
  events for that workflow are appended to the timeline in sequence order
  (de-duplicated against history already loaded, since the same event may
  arrive both in the history fetch and the live stream around the
  subscription boundary).
- The list view subscribes to a filtered stream matching its active filter
  (type/status/namespace); incoming events update the corresponding row's
  status or insert a newly-matching workflow.
- The feed view can subscribe to the firehose for unfiltered observability.

**Reconnection discipline.** On socket drop, the manager reconnects with
backoff and the active view *resyncs*: the detail view re-fetches history (or
requests events after its last-seen sequence, if AW's protocol supports an
`after_seq` cursor — `apps/web`'s assistant socket already uses exactly this
pattern), and the list view re-runs its query. The operator sees a
"reconnecting" indicator, never a silently dead stream. This is non-optional:
an operator who left the dashboard open overnight must wake to a live view,
not a frozen one.

### Production States

Every async surface has four states, rendered explicitly:

- **Loading** — skeletons for the list and timeline, not bare spinners where
  layout can be reserved.
- **Empty** — "no workflows match this filter" / "this workflow has no events
  yet", distinct from loading and from error.
- **Error** — the query/history failure surfaced with the cause and a retry
  affordance, never a blank panel.
- **Live/connected vs disconnected** — a persistent connection indicator so
  the operator always knows whether what they see is current.

## Structure

```
apps/aion-dashboard/package.json            workspace member, house scripts (dev/build/check/typecheck)
apps/aion-dashboard/vite.config.ts          Vite + React + Tailwind v4, base-path configurable for AW hosting
apps/aion-dashboard/biome.json              Biome config (100-char), matching apps/web
apps/aion-dashboard/tsconfig.json           TS config matching apps/web
apps/aion-dashboard/index.html              SPA entry
apps/aion-dashboard/src/main.tsx            React root mount
apps/aion-dashboard/src/app/App.tsx         shell: router, providers, layout
apps/aion-dashboard/src/app/routes.tsx      / (list), /workflows/:id (detail)
apps/aion-dashboard/src/app/providers.tsx   QueryClient + namespace + websocket providers

apps/aion-dashboard/src/lib/api/client.ts   typed REST client: listWorkflows/queryWorkflows, getHistory, listNamespaces
apps/aion-dashboard/src/lib/api/websocket.ts  Aion event WebSocket manager (subscribe/reconnect/resync)
apps/aion-dashboard/src/lib/api/index.ts    barrel re-export
apps/aion-dashboard/src/types/generated/    TS types generated from aion-core/aion-proto (never hand-edited)
apps/aion-dashboard/src/types/index.ts      local view types + re-export of generated wire types

apps/aion-dashboard/src/features/namespace/context/NamespaceContext.tsx  selected namespace, scoping
apps/aion-dashboard/src/features/namespace/components/NamespaceSelector.tsx
apps/aion-dashboard/src/features/namespace/index.ts

apps/aion-dashboard/src/features/workflow-list/components/WorkflowList.tsx
apps/aion-dashboard/src/features/workflow-list/components/WorkflowRow.tsx
apps/aion-dashboard/src/features/workflow-list/components/FilterBar.tsx
apps/aion-dashboard/src/features/workflow-list/components/Pagination.tsx
apps/aion-dashboard/src/features/workflow-list/hooks/useWorkflowQuery.ts   React Query against query API
apps/aion-dashboard/src/features/workflow-list/hooks/useLiveListUpdates.ts filtered WS subscription → row updates
apps/aion-dashboard/src/features/workflow-list/types.ts
apps/aion-dashboard/src/features/workflow-list/index.ts

apps/aion-dashboard/src/features/workflow-detail/components/WorkflowDetail.tsx
apps/aion-dashboard/src/features/workflow-detail/components/EventTimeline.tsx
apps/aion-dashboard/src/features/workflow-detail/components/TimelineEntry.tsx
apps/aion-dashboard/src/features/workflow-detail/components/ActivityGroup.tsx
apps/aion-dashboard/src/features/workflow-detail/components/PayloadView.tsx
apps/aion-dashboard/src/features/workflow-detail/hooks/useWorkflowHistory.ts  React Query against history API
apps/aion-dashboard/src/features/workflow-detail/hooks/useLiveWorkflowEvents.ts per-workflow WS subscription + resync
apps/aion-dashboard/src/features/workflow-detail/lib/timeline.ts             event→timeline-model projection + correlation
apps/aion-dashboard/src/features/workflow-detail/types.ts
apps/aion-dashboard/src/features/workflow-detail/index.ts

apps/aion-dashboard/src/features/live-feed/hooks/useEventSubscription.ts     shared subscribe/resync lifecycle
apps/aion-dashboard/src/features/live-feed/hooks/useConnectionStatus.ts
apps/aion-dashboard/src/features/live-feed/components/ConnectionIndicator.tsx
apps/aion-dashboard/src/features/live-feed/components/FirehoseFeed.tsx
apps/aion-dashboard/src/features/live-feed/index.ts

apps/aion-dashboard/src/components/ui/                shadcn/Radix primitives (badge, select, table, skeleton, ...)
apps/aion-dashboard/src/components/StatusBadge.tsx    WorkflowStatus → badge
apps/aion-dashboard/src/components/EventIcon.tsx      event kind → icon + colour
apps/aion-dashboard/src/components/EmptyState.tsx     shared empty panel
apps/aion-dashboard/src/components/ErrorState.tsx     shared error panel + retry
apps/aion-dashboard/src/components/LoadingSkeleton.tsx
```

## Constraints

- **CO1** — Match the `apps/web` stack exactly: React 19, TypeScript, Vite,
  Tailwind v4, Bun, Biome (100-char). No new framework, no alternate styling
  system, no alternate state library. shadcn/Radix primitives for UI, TanStack
  Query for server state.
- **CO2** — Wire types come from the house generated-types pipeline (Rust
  `#[derive(TS)]` → `src/types/generated/`). The dashboard never hand-defines
  a type that `aion-core`/`aion-proto` owns, and never hand-edits a generated
  file.
- **CO3** — The dashboard consumes the AW API/WebSocket contract; it defines
  no server endpoint, no WebSocket protocol, and no asset-hosting mechanism.
  Any not-yet-pinned contract detail is isolated in the api module so a change
  is a one-file edit.
- **CO4** — Feature-folder convention from `apps/web`: each feature is
  `components/` + `hooks/` (+ `context/`/`lib/` as needed) + `types.ts` +
  `index.ts`. Barrels re-export; logic lives in named files.
- **CO5** — Server state goes through TanStack Query; live updates flow
  through the WebSocket manager and update query caches or local view state.
  No bespoke fetch-and-store-in-useState for server data.
- **CO6** — Every async surface renders explicit loading, empty, and error
  states. No surface may spin indefinitely or blank out on failure.
- **CO7** — The WebSocket manager reconnects with bounded backoff on drop and
  the active view resyncs (re-fetch history/query, or replay after last-seen
  sequence where the protocol supports it). A dropped socket never leaves a
  silently stale view; a connection indicator is always present.
- **CO8** — The event timeline is ordered strictly by event sequence number
  and de-duplicates events that arrive via both history fetch and live stream.
- **CO9** — Biome lint + format clean (`bun run check`) and `tsc --noEmit`
  clean. No lint-disable comments to silence findings.
- **CO10** — Namespace selection scopes every query and every subscription;
  switching namespace re-scopes both. No view issues an unscoped query when a
  namespace is selected.

## Non-Goals

- **No server.** The REST API, WebSocket endpoint, and static-asset hosting
  are cluster **AW**. This cluster builds only the browser UI that consumes
  them.
- **No engine.** Lifecycle, replay, timers, signals, queries are clusters
  **AE/AD/AT**. The dashboard reads the events those produce; it never drives
  execution.
- **No client SDK.** Programmatic start/signal/query/cancel callers are
  cluster **AL**. The dashboard is the *human* UI, read-only over the
  monitoring surface. (Operator *actions* — terminate/cancel a workflow from
  the UI — are deliberately out of scope here; this cluster is observability.
  If actions are wanted later, they layer on AL's command API.)
- **No authentication system.** Auth/session is the server's concern (AW);
  the dashboard carries whatever credential the server expects on requests and
  the socket, but defines no auth flow.
- **No bespoke charting/metrics dashboards.** This is workflow observability
  (list, history, live feed), not a Grafana-style metrics surface. Aggregate
  metrics are a later, separate concern.
- **No mobile-first responsive design pass.** Operator console, desktop-first;
  it should not break on a narrow window, but a dedicated mobile layout is out
  of scope.
