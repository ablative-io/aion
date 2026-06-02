# Aion-Dashboard — Checklist

## App Scaffold and Stack

- [ ] **C1** — apps/aion-dashboard exists as a workspace member with package.json declaring house scripts (dev, build, check, format, typecheck) matching apps/web.
- [ ] **C2** — The stack matches apps/web: React 19, TypeScript, Vite, Tailwind v4, Bun, Biome (100-char line width); no alternate framework, styling system, or state library is introduced.
- [ ] **C3** — vite.config.ts builds a static SPA bundle and supports a configurable base path so AW can serve it under a sub-path.
- [ ] **C4** — src/main.tsx mounts the React root and index.html is the SPA entry; bun run build produces a static bundle and tsc --noEmit passes.
- [ ] **C5** — shadcn/Radix UI primitives are available under src/components/ui and used for all interactive controls.

## Types and REST Client

- [ ] **C6** — Wire types (Event, WorkflowSummary, WorkflowStatus, WorkflowFilter, namespace) are sourced from the generated-types pipeline (Rust #[derive(TS)] → src/types/generated/), not hand-defined, and generated files are not hand-edited.
- [ ] **C7** — A typed REST client exposes listWorkflows/queryWorkflows (filter + pagination), getHistory(workflowId), and listNamespaces, each returning typed results.
- [ ] **C8** — The REST client returns typed errors and the not-yet-pinned AW contract details (paths, parameter names, pagination shape) are isolated in this one module.
- [ ] **C9** — Local-only view models (timeline entry, filter form state) are hand-written in feature types.ts and kept distinct from generated wire types.

## WebSocket Manager

- [ ] **C10** — A singleton WebSocket manager connects to the server's event stream and exposes subscribe-by-filter for per-workflow, filtered, and firehose subscriptions.
- [ ] **C11** — The manager parses incoming frames into typed Event values and dispatches them to registered handlers.
- [ ] **C12** — On disconnect the manager reconnects with bounded backoff and notifies connect/disconnect handlers driving a connection indicator.
- [ ] **C13** — On reconnect the active subscription is re-established and a resync is triggered (re-fetch or replay after last-seen sequence where the protocol supports it).
- [ ] **C14** — A useConnectionStatus hook exposes connected/reconnecting/disconnected state for the UI.

## Workflow List View

- [ ] **C15** — The list view loads workflow summaries via TanStack Query against the query API and renders them as rows (id, type, status, start/end time).
- [ ] **C16** — A filter bar filters by workflow type, status, and time range, driving the query.
- [ ] **C17** — The list paginates against the server's pagination contract and does not load full histories.
- [ ] **C18** — The list renders explicit loading (skeleton rows), empty (no workflows match this filter), and error (cause + retry) states.
- [ ] **C19** — A filtered WebSocket subscription updates rows live: status transitions update the corresponding row and newly-matching workflows are inserted.
- [ ] **C20** — Each row links to the workflow detail view.

## Workflow Detail and Timeline

- [ ] **C21** — The detail view loads a single workflow's full event history via TanStack Query against the history API.
- [ ] **C22** — Events project into a timeline model ordered strictly by event sequence number.
- [ ] **C23** — Each Event variant renders a timeline entry: lifecycle (started/completed/failed/cancelled), activity (scheduled/started/completed/failed), timer (started/fired/cancelled), signal (received), child workflow (started/completed/failed).
- [ ] **C24** — Activity events are correlated by ActivityId into one expandable group (retries shown as repeated failed attempts); timer events correlate by TimerId; child-workflow events correlate by child id.
- [ ] **C25** — Each entry shows its envelope (sequence number, recorded timestamp), a kind icon and colour, a one-line summary, and an expandable payload view.
- [ ] **C26** — Child-workflow entries link to the child workflow's own detail view.
- [ ] **C27** — The detail view renders explicit loading (skeleton timeline), empty (no events yet), and error (cause + retry) states.

## Live Events and Resync

- [ ] **C28** — A per-workflow WebSocket subscription appends live events to the timeline in sequence order as the execution progresses.
- [ ] **C29** — Live-appended events are de-duplicated by sequence number against events already loaded from the history fetch.
- [ ] **C30** — The timeline reflects the terminal outcome (completed/failed/cancelled/timed-out) when the closing lifecycle event arrives.
- [ ] **C31** — On socket drop the detail view resyncs (re-fetch history or replay after last-seen sequence) and the list view re-runs its query, with a reconnecting indicator visible throughout.
- [ ] **C32** — A firehose feed renders an unfiltered live event stream for observability.

## Namespace, Shared States, and Quality

- [ ] **C33** — A namespace context holds the selected namespace and a selector lets the operator switch it.
- [ ] **C34** — Every query and every subscription is scoped by the selected namespace; switching namespace re-scopes both the active query and the live subscription.
- [ ] **C35** — Shared StatusBadge, EventIcon, EmptyState, ErrorState (with retry), LoadingSkeleton, and ConnectionIndicator components exist and are reused across views.
- [ ] **C36** — The app shell wires routing (/ list, /workflows/:id detail) and the QueryClient, namespace, and websocket providers.
- [ ] **C37** — bun run check (Biome lint + format) and tsc --noEmit pass clean with no lint-disable comments added to silence findings.
