# Implementation Spec — Nested child swim-lanes in the run view (recursive embedded run view + retention-API transcripts)

Repository: `/Users/tom/Developer/ablative/aion`. **Base branch: `lane/t229-transcript-retention`** (transcript retention landed there; commit `561b29e2`).
Worktree (create first, work ONLY inside it):
`git -C /Users/tom/Developer/ablative/aion worktree add /Users/tom/Developer/ablative/aion/.worktrees/t230-nested-child-lanes -b lane/t230-nested-child-lanes lane/t229-transcript-retention`
Every cargo command: `CARGO_TARGET_DIR=/Users/tom/Developer/ablative/aion/target`. Gate outputs → `<worktree>/t230-gates/` (untracked) with an exit-code manifest. Never build in /tmp.

---

## 0. VERIFY-FIRST answer (verified by reading; design assumes these facts)

**Q: Does the parent's recorded child-spawn event carry the child's workflow id on the wire?** **YES — no server change needed.**

1. Server event structs: `Event::ChildWorkflowStarted { child_workflow_id: WorkflowId, workflow_type, input, package_version, .. }` and `ChildWorkflowCompleted`/`ChildWorkflowFailed`/`ChildWorkflowCancelled` all carry `child_workflow_id: WorkflowId` — `crates/aion-core/src/event.rs:416-457`.
2. It reaches the console wire types: the ts-rs generated union carries `child_workflow_id: WorkflowId` on all four variants — `apps/aion-ops-console/src/types/generated/index.ts:528-584`.
3. The console timeline projection already lifts it: `patchChild` keys entries on `event.data.child_workflow_id` (`apps/aion-ops-console/src/features/workflow-detail/lib/timeline.ts:380-407`, entry field at `lib/timeline.ts:422`, type at `features/workflow-detail/types.ts:92-101`), and the swimlane layout already stamps `childWorkflowId` onto child bars (`swimlane/laneLayout.ts:131-135`, bar field at `laneLayout.ts:42`).
4. The id is the child's REAL addressable workflow id: the engine reads the child's own history under exactly that id — `store.read_history(child_workflow_id)` at `crates/aion/src/engine/reload.rs:292` (also `crates/aion/src/engine/startup_sweeps.rs:62`); the recorder records the same id the child runs under (`crates/aion/src/durability/recorder.rs:846-862`). The console history endpoint `POST /workflows/describe` (route: `crates/aion-server/src/api/http/router.rs:219`; client: `getHistory` at `apps/aion-ops-console/src/lib/api/client.ts:220-229`) therefore serves the child's full history for that id, and the per-workflow live subscription (`filter { kind: 'workflow', namespace, workflowId }`, `hooks/useLiveWorkflowEvents.ts:57-63`) works for the child id too.

**Transcript retention read API (on the base branch, console-untouched there):**
- `POST /workflows/transcript` → `{ events: ActivityEvent[] }` in `store_seq` order, body `{ namespace, workflow_id, activity_id, attempt, from_seq? }`; `POST /workflows/transcripts` → `{ streams: [{ activity_id, attempt, head }] }` — `crates/aion-server/src/api/http/transcripts.rs:35-143` (branch `lane/t229-transcript-retention`), routed always-mounted next to `/workflows/attempts` (`crates/aion-server/src/api/http/router.rs`, +2 routes on the lane branch). Empty list/array is the honest answer for pre-retention runs; namespace-gated identically to the WS transcript subscription. The documented client contract: REST fetch first, then WS attach with `after_seq` = last fetched `store_seq` (`transcripts.rs:8-12`); the WS dedups the splice seam on `store_seq` and the console fold also drops persisted dupes (`features/transcript/hooks/useTranscript.ts:144-148`).
- The transcript WS manager today starts its cursor at `null` (`lib/api/transcript-stream.ts:117-123`) and only sends `after_seq` after applying an event (`transcript-stream.ts:252-256`) — it needs an `initialAfterSeq` option (work package C).

**Lazy-subscription mechanics already exist:** `useLiveWorkflowEvents` subscribes only while mounted + non-terminal (`hooks/useLiveWorkflowEvents.ts:57-63`), and the `useEventSubscription` effect returns the manager unsubscribe on unmount (`features/live-feed/hooks/useEventSubscription.ts:99-114`; `lib/api/websocket.ts:118-140`). `useWorkflowHistory` (react-query) backfills full durable history on mount (`hooks/useWorkflowHistory.ts:29-45`) and the live hook is gated `enabled: historyQuery.isSuccess` (`swimlane/WorkflowDetailView.tsx:36-41`) — **mounting the real run view on expand IS "backfill first, then live tail"; unmounting on collapse IS unsubscribe.** A terminal child never opens a socket at all (`useLiveWorkflowEvents.ts:58`).

**Namespace assumption:** children run under the parent's namespace; every embedded fetch/subscription reuses the selected namespace exactly like the existing child drill-link does (`components/TimelineEntry.tsx:73-75` navigates within the same selection).

**Server-side work: NONE.** This lane is console-only ⇒ the committed embed **must** be regenerated (mandatory gate, §6).

---

## 1. Work package A — extract the presentational run view (break the recursion cycle)

### 1.1 New file `apps/aion-ops-console/src/features/workflow-detail/swimlane/WorkflowDetailViewContent.tsx`
Move VERBATIM from `swimlane/WorkflowDetailView.tsx` (395 lines today): `WorkflowDetailViewContent` (`WorkflowDetailView.tsx:148-298`), `ContentProps` (`:70-97`), `isReopenable` (`:105-107`), `useSelectionState` (`:125-146`), `FailureContextPanel` (`:306-330`), `ViewToggle`/`ToggleButton` (`:332-368`), `StatusBadge` (`:370-392`). This keeps both files well under the 500-code-line law once the additions below land.

Then EXTEND `ContentProps` with three optional props (all `undefined`-safe so every existing call site/test is untouched):
- `embedded?: boolean` — render the compact ancestry header instead of the page `<h1>` header block (`WorkflowDetailView.tsx:231-258`).
- `ancestry?: readonly string[]` — the ancestor workflow-id chain, outermost first (empty/omitted at the root).
- `renderChildRun?: (childWorkflowId: string) => ReactNode` — the injected recursive embed point, passed through to `Swimlane` (§2). Injection (not a direct import) keeps the module graph acyclic: `WorkflowDetailView.tsx → {Content, EmbeddedRunView}`, `EmbeddedRunView → Content`, `Content → Swimlane`.

Embedded header (when `embedded` is true), replacing the `<header>` at `WorkflowDetailView.tsx:231-258`:
```tsx
<header className="flex flex-wrap items-center justify-between gap-2">
  <nav aria-label="Workflow ancestry" className="flex min-w-0 flex-wrap items-center gap-1 text-xs">
    {(ancestry ?? []).map((id) => (
      <Fragment key={id}>
        <Link className="truncate font-mono text-muted-foreground hover:text-foreground" to={workflowDetailHref(id)}>{id}</Link>
        <span aria-hidden="true" className="text-muted-foreground">›</span>
      </Fragment>
    ))}
    <span className="truncate font-mono text-foreground">{workflowId}</span>
  </nav>
  <div className="flex items-center gap-2">
    <StatusBadge isLive={isLive} isTerminal={isTerminal} terminalOutcome={terminalOutcome} />
    {reopenable ? <Button data-testid="reopen-open" …>Reopen</Button> : null}
    <ViewToggle mode={mode} onChange={setMode} />
    <Link className="text-primary text-xs underline-offset-2 hover:underline" data-testid="open-full-view" to={workflowDetailHref(workflowId)}>Open full view</Link>
  </div>
</header>
```
`workflowDetailHref` from `@/app/routePaths` (`app/routePaths.ts:28-31`); use react-router `Link` (the app always renders under the router — `app/routes.tsx:91-116`); the "Open full view" link is the **zoom-into-lane** navigation, the breadcrumb is the **ancestor trail** — together the "minimal navigation for deep trees". The non-embedded branch keeps today's header byte-identical. Everything else in Content (Scrubber, Swimlane, EventTimeline, DetailSheet, AttemptNavigator, ReopenDiff) renders in BOTH modes — the embedded view is the real component, **not a lite fork**: `AttemptNavigator` at `WorkflowDetailView.tsx:287` carries the transcript + intervention pipeline (`swimlane/AttemptNavigator.tsx:55-115`, one shared `useInterventionController` — `features/transcript/components/InterventionControls.tsx:104-134`) and it is keyed by the `workflowId` prop, so **intervention works at any depth** with zero changes.

Pass `renderChildRun` to `<Swimlane … renderChildRun={renderChildRun} />` (Content's swimlane branch, `WorkflowDetailView.tsx:263-272`). Do NOT pass it to `EventTimeline` (list mode keeps its existing child link, `components/TimelineEntry.tsx:73-75` — switch that raw `href` to `workflowDetailHref` while you are there ONLY if it is a one-line swap; otherwise leave it).

### 1.2 `swimlane/WorkflowDetailView.tsx` (rewritten, small)
Keeps the router-connected wrapper (`:35-68`) plus:
```tsx
import { EmbeddedRunView } from './EmbeddedRunView';
…
<WorkflowDetailViewContent
  … (existing props unchanged)
  renderChildRun={(childId) => (
    <EmbeddedRunView ancestry={[workflowId]} namespace={namespace} workflowId={childId} />
  )}
/>
```
Re-export for import stability (`Swimlane.test.tsx:9` and `swimlane/index.ts:14` import Content from here): `export { WorkflowDetailViewContent } from './WorkflowDetailViewContent';` alongside `export { WorkflowDetailView }`.

### 1.3 New file `swimlane/EmbeddedRunView.tsx`
```tsx
/** Pure cycle guard: an id already in its own ancestor chain must not re-embed. */
export function isEmbedCycle(workflowId: string, ancestry: readonly string[]): boolean {
  return ancestry.includes(workflowId);
}

export type EmbeddedRunViewProps = {
  workflowId: WorkflowId;
  namespace: string | null;
  /** Ancestor chain, outermost first; also the recursion/cycle guard. */
  ancestry: readonly string[];
};

/** Presentational cycle notice (SSR-testable). */
export function CycleNotice({ workflowId }: { workflowId: string }) → renders an
  <EmptyState title="Recursive child reference" description={`Workflow ${workflowId} is already expanded above; open it in its own view instead.`} data-testid="embed-cycle" /> shape (wrap EmptyState; if EmptyState takes no data-testid, wrap in <div data-testid="embed-cycle">).

export function EmbeddedRunView({ workflowId, namespace, ancestry }: EmbeddedRunViewProps) {
  if (isEmbedCycle(workflowId, ancestry)) { return <CycleNotice workflowId={workflowId} />; }
  return <EmbeddedRunViewBody … />;
}

function EmbeddedRunViewBody({ workflowId, namespace, ancestry }: EmbeddedRunViewProps) {
  // EXACT mirror of the route wrapper's data composition (WorkflowDetailView.tsx:36-41):
  const historyQuery = useWorkflowHistory({ workflowId });                    // full durable backfill FIRST
  const live = useLiveWorkflowEvents({ enabled: historyQuery.isSuccess, history: historyQuery.data ?? [], workflowId }); // live tail attaches only after history
  return (
    <WorkflowDetailViewContent
      ancestry={ancestry}
      embedded
      error={historyQuery.error}
      history={live.events}
      isError={historyQuery.isError}
      isLive={!live.isTerminal && namespace !== null}
      isLoading={historyQuery.isLoading || historyQuery.isPending}
      isTerminal={live.isTerminal}
      namespace={namespace}
      onRetry={() => void historyQuery.refetch()}
      renderChildRun={(childId) => (
        <EmbeddedRunView ancestry={[...ancestry, workflowId]} namespace={namespace} workflowId={childId} />
      )}
      terminalOutcome={live.terminalOutcome}
      timeline={live.timeline}
      workflowId={workflowId}
    />
  );
}
```
NO selection/mode/scrub setters are passed ⇒ Content falls back to internal `useState` (`useSelectionState`, `WorkflowDetailView.tsx:125-146` behavior), so a nested view's selection never fights the parent's URL params (`hooks/useWorkflowSelectionParams.ts:48+` stays root-only). Self-recursion inside one module = no import cycle; grandchildren expand identically to any depth. Hooks are context-backed (`useNamespace` inside `useWorkflowHistory`, `hooks/useWorkflowHistory.ts:34`; react-query) — fine in-app because the whole tree renders under the providers (`app/providers.tsx`).

Mount/unmount semantics deliver the lane contract: **expand ⇒ mount ⇒ react-query history fetch (an hour-late expand shows everything — events are durable) then live subscription; collapse ⇒ unmount ⇒ `useEventSubscription` cleanup unsubscribes (`useEventSubscription.ts:99-114`); only expanded lanes ever subscribe; terminal children never subscribe (`useLiveWorkflowEvents.ts:57-63`).**

### 1.4 Exports
`swimlane/index.ts` (re-exports only): add `export { CycleNotice, EmbeddedRunView, isEmbedCycle } from './EmbeddedRunView';` and keep line 14's Content export working via §1.2's re-export. `features/workflow-detail/index.ts`: add `EmbeddedRunView` to the public surface next to the existing swimlane exports.

---

## 2. Work package B — expandable child lanes in the Swimlane

### 2.1 `swimlane/laneLayout.ts`
Add to `SwimlaneLane` (`laneLayout.ts:52-58`): `/** Child workflow id for a child lane (the expansion target); null otherwise. */ childWorkflowId: string | null;`.
- `appendBar` (`laneLayout.ts:168-181`): extend signature `appendBar(lanes, id, label, bar, childWorkflowId: string | null = null)` and set it on the lane literal at creation (`{ id, kind, label, childWorkflowId, bars: [] }`).
- The child arm (`laneLayout.ts:131-136`) passes `entry.childWorkflowId`; the lifecycle/signal/generic lane literals (`laneLayout.ts:144-155`) gain `childWorkflowId: null`.

### 2.2 `swimlane/Swimlane.tsx`
New props on `SwimlaneProps` (`Swimlane.tsx:16-30`):
- `renderChildRun?: (childWorkflowId: string) => ReactNode` — when absent, NO expansion affordance renders (pure layout tests and legacy call sites unchanged).
- `initialExpandedChildren?: readonly string[]` — test-only seed (same pattern as `initialReopenOpen`, `WorkflowDetailView.tsx:84`), lane-ids or child ids? **Use child workflow ids** (stable, test-friendly).

State beside `collapsed` (`Swimlane.tsx:44`): `const [expandedChildren, setExpandedChildren] = useState<ReadonlySet<string>>(() => new Set(initialExpandedChildren ?? []));` + `toggleChildRun(id)` twin of `toggleLane` (`Swimlane.tsx:54-64`).

**Placement decision (recorded):** the expanded child region renders **beneath the swimlane's horizontal-scroll `<section>`, inside the Swimlane component root** — NOT inside the `minWidth: LANE_LABEL_WIDTH + trackWidth` scrollport div (`Swimlane.tsx:73-94`). Rendering a full run view inside the scrollport would pin it to the parent's dense-rank width (unreadable when the parent is scrolled right, pathological at depth); each embedded run view brings its own `overflow-x-auto` swimlane anyway. Structure:
```tsx
return (
  <div className="space-y-2">
    <section aria-label="Workflow swimlane" …>  {/* existing scrollport, unchanged */} </section>
    {layout.lanes
      .filter((lane) => lane.childWorkflowId !== null && expandedChildren.has(lane.childWorkflowId) && renderChildRun !== undefined)
      .map((lane) => (
        <section
          aria-label={`Child run ${lane.childWorkflowId}`}
          className="rounded-xl border border-border border-l-4 border-l-primary/40 bg-surface-base p-3 pl-4"
          data-testid={`child-run:${lane.childWorkflowId}`}
          key={`child-run:${lane.id}`}
        >
          {renderChildRun(lane.childWorkflowId as string)}
        </section>
      ))}
  </div>
);
```
(The left accent border + indent reads as "this region belongs to that lane"; grandchild expansions nest visually by the same rule inside the embedded view's own Swimlane.)

`LaneRow` (`Swimlane.tsx:131-181`): add props `childWorkflowId: string | null`, `runExpanded: boolean`, `onToggleChildRun: (() => void) | null` (null when no `renderChildRun`). For child lanes with a toggle, render next to the existing label button (keep the collapse toggle as-is) a second compact button in the label cell:
```tsx
{childWorkflowId !== null && onToggleChildRun !== null ? (
  <button
    aria-expanded={runExpanded}
    aria-label={`${runExpanded ? 'Collapse' : 'Expand'} child run ${childWorkflowId}`}
    className="shrink-0 rounded px-1 text-muted-foreground text-xs hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    data-testid={`expand-child:${childWorkflowId}`}
    onClick={onToggleChildRun}
    type="button"
  >{runExpanded ? '▾' : '▸'}</button>
) : null}
```
(Adjust the label-cell flex: the existing `<button>` at `Swimlane.tsx:148-162` stays; wrap the two buttons in the row's existing `flex items-stretch` `<li>` — the expand button sits before or after the label button, implementer's call, but it must be a SEPARATE button so lane-collapse and run-expand stay independent.)

Collapse semantics: toggling the run-expand OFF removes the region ⇒ the embedded view unmounts ⇒ its subscription(s) close (§0). The existing per-lane bar-collapse (`collapsed` set) is untouched and independent.

---

## 3. Work package C — transcripts from the retention read API (REST history + WS tail)

### 3.1 New file `apps/aion-ops-console/src/lib/api/transcript-read.ts`
The REST twin consumption of the lane-#229 pair. Shape it on the `ApiClient` transport precedents (`client.ts:641-665` request, `:728-748` headers; contract object precedent `client-transport.ts:6-60`):
```ts
export const TRANSCRIPT_READ = {
  fetch: '/workflows/transcript',
  streams: '/workflows/transcripts',
} as const;

export type TranscriptReadOptions = { baseUrl?: string; fetchImpl?: FetchFn; credentials?: ApiCredentials };
export type TranscriptFetchParams = TranscriptTarget & { fromSeq?: number | undefined };
export type RetainedStreamHead = { activityId: ActivityId; attempt: number; head: number };

export class TranscriptReadClient {
  constructor(options: TranscriptReadOptions = {}) { … stripTrailingSlash(baseUrl), fetchImpl ?? fetch, credentials }
  async fetchTranscript(params: TranscriptFetchParams): Promise<ActivityEvent[]>
  async listStreams(namespace: Namespace, workflowId: WorkflowId): Promise<RetainedStreamHead[]>
}
```
- Request bodies (server snake_case per `transcripts.rs:35-67`): fetch → `{ namespace, workflow_id, activity_id, attempt, ...(fromSeq === undefined ? {} : { from_seq: fromSeq }) }`; streams → `{ namespace, workflow_id }`. Both `POST`, `content-type: application/json`.
- Headers: extract a shared helper into `lib/api/client-transport.ts`: `export function buildScopedHeaders(credentials: ApiCredentials | undefined): Headers` implementing exactly `ApiClient.buildHeaders`'s post-merge body (`client.ts:728-748`: content-type json, `appendHeaders(credentials?.headers)`, `authorization: Bearer`, `x-aion-subject`, `x-aion-namespaces` joined). Refactor `ApiClient.buildHeaders` to `return buildScopedHeaders(mergeCredentials(this.credentials, options.credentials));` (shrinks the oversized client.ts; do NOT otherwise grow client.ts — it is already past the LOC law, which is why this lane's REST reads live in a new file).
- Responses: non-OK → `throw apiErrorFromResponse(response.status, await readJson(response).catch(() => null))` (exactly `client.ts:657-660`; `readJson`/`apiErrorFromResponse` are exported from `client-normalize.ts`). OK → validate the envelope like `normalizeAttempts` does (`client.ts:782-801`): fetch requires `Array.isArray(body.events)` else `throw new ApiError(200, 'workflows/transcript response missing an events array')`; keep events verbatim (they are the ts-rs `ActivityEvent` with `store_seq` — `transcripts.rs:53-57`). streams requires `Array.isArray(body.streams)`; map `{ activity_id, attempt, head }` → `{ activityId, attempt, head }` with `typeof` number guards, skipping malformed rows is NOT allowed — a malformed row throws (truth-first).
- Export from `lib/api/index.ts` (`TranscriptReadClient`, `RetainedStreamHead`, `TranscriptFetchParams`, `TRANSCRIPT_READ`).

### 3.2 `lib/config/clients.ts`
Add beside `createConfiguredApiClient` (`clients.ts:32-41`):
```ts
export function createConfiguredTranscriptReader(options: ConfiguredClientOptions = {}): TranscriptReadClient
```
identical config/credentials plumbing (`getOpsConsoleConfig` + `buildCredentials(config, options.namespace)` + `apiBaseUrl`).

### 3.3 `lib/api/transcript-stream.ts` — seed the resume cursor
- `TranscriptStreamManagerOptions` (`transcript-stream.ts:83-91`): add `/** Resume cursor seeded from a REST backfill: the subscribe frame carries after_seq immediately, so the WS serves only the live tail past the fetched history. */ initialAfterSeq?: number;`.
- Constructor (`:125-136`): `this.lastAppliedSeq = options.initialAfterSeq ?? null;` (field decl at `:117-123`; `sendSubscribe` at `:252-256` then emits `after_seq` on the FIRST connect). No other change; reconnect semantics unchanged.
- `createConfiguredTranscriptStream` (`lib/config/clients.ts:110-121`): signature → `(target: TranscriptTarget, config: OpsConsoleConfig = getOpsConsoleConfig(), initialAfterSeq?: number)`; pass `...(initialAfterSeq === undefined ? {} : { initialAfterSeq })` into the manager options.

### 3.4 `features/transcript/hooks/useTranscript.ts` — REST backfill first, WS tail second
Extend (all additions; existing fold logic and entry types untouched — `useTranscript.ts:135-284`):
- New pure helper (exported, unit-tested):
```ts
/** Fold a REST-retained transcript (store_seq order) into entries + the resume cursor. */
export function backfillEntries(events: readonly ActivityEvent[]): { entries: TranscriptEntry[]; lastSeq: number | undefined } {
  let entries: TranscriptEntry[] = [];
  let lastSeq: number | undefined;
  let deltaId = 0;
  for (const event of events) {
    entries = foldTranscriptEvent(entries, event, deltaId);
    deltaId += 1;
    if (typeof event.store_seq === 'number') { lastSeq = lastSeq === undefined ? event.store_seq : Math.max(lastSeq, event.store_seq); }
  }
  return { entries, lastSeq };
}
```
- `UseTranscriptOptions` (`:91-96`): `createManager?: (target: TranscriptTarget, initialAfterSeq?: number) => AionTranscriptStreamManager;` (default: `(target, seq) => createConfiguredTranscriptStream(target, undefined-config-default, seq)` — adapt to §3.3's signature) and `fetchRetained?: (target: TranscriptTarget) => Promise<ActivityEvent[]>;` (default: `(t) => createConfiguredTranscriptReader({ namespace: t.namespace }).fetchTranscript(t)`).
- `UseTranscriptResult` (`:98-105`): add `backfillState: 'loading' | 'ready' | 'error';` and `backfillError: Error | null;`.
- Effect rework (`:298-332`), keeping the `key`/`parseTargetKey` remount discipline (`:296`, `:338-346`):
```
key === '' → reset all (as today) + backfillState 'ready', backfillError null.
else:
  reset entries, deltaCounter, backfillError; setBackfillState('loading');
  let cancelled = false; let teardown: (() => void) | null = null;
  const attach = (initialAfterSeq: number | undefined) => {
    const manager = createManager(resolved, initialAfterSeq);
    … existing listener/subscribe/status/error wiring (:314-325), captured into teardown = () => { unsubscribeStream(); unsubscribeStatus(); unsubscribeError(); };
  };
  fetchRetained(resolved).then((events) => {
    if (cancelled) return;
    const { entries: retained, lastSeq } = backfillEntries(events);
    deltaCounter.current = events.length;              // keep live keys unique past the backfill
    setEntries(retained);
    setBackfillState('ready');
    attach(lastSeq);                                    // REST history + WS tail (after_seq = lastSeq); lastSeq undefined ⇒ full WS replay (pre-retention run — honest)
  }).catch((error) => {
    if (cancelled) return;
    setBackfillError(error instanceof Error ? error : new Error(String(error)));
    setBackfillState('error');
    attach(undefined);                                  // resilient fallback: the WS durable replay still serves full history; the REST failure stays VISIBLE
  });
  return () => { cancelled = true; teardown?.(); };
```
Dedup safety across the seam is double-covered: the server suppresses `store_seq <= after_seq` (`transcripts.rs:8-12`) AND `foldTranscriptEvent` drops persisted duplicates (`useTranscript.ts:144-148`), so a race re-delivery cannot double-render. No `unwrap`-style shortcuts; errors always land in visible state.
- `features/transcript/components/TranscriptPanel.tsx`: `TranscriptPanel` (`:38-49`) passes the two new fields; `TranscriptPanelContentProps` (`:51-56`) gains `backfillError: Error | null` (and optionally `backfillState`); render a warning block mirroring the socket-error block (`:89-97`) with `data-testid="transcript-backfill-error"` and message `Retained transcript fetch failed: ${backfillError.message} — showing the live-socket replay instead.` Update `AssistantSessionView`'s usage only if props are required — make them optional-with-default (`backfillError = null`) so `assistant/components/AssistantSessionView.tsx:225` compiles untouched.
- If `useTranscript.ts` exceeds 500 code lines, move `foldTranscriptEvent` + its private helpers (`:135-284`) to a new `features/transcript/lib/foldTranscript.ts` and re-export from the hook module so `__tests__/foldTranscriptEvent.test.ts` and `features/transcript/index.ts:12` imports stay valid.

### 3.5 Retained-stream enumeration consumption — attempt-row badge
New `features/transcript/hooks/useRetainedStreams.ts`, modeled line-for-line on `useActivityAttempts` (plain effect + injectable client, `hooks/useActivityAttempts.ts:55-101`):
```ts
export type UseRetainedStreamsOptions = { workflowId: WorkflowId | null; namespace: Namespace | null; reader?: Pick<TranscriptReadClient, 'listStreams'> };
export type UseRetainedStreamsResult = { streams: RetainedStreamHead[]; loadState: 'idle' | 'loading' | 'ready' | 'error'; loadError: Error | null };
```
Loads once per `(namespace, workflowId)` identity; a failure is visible state (`loadError`), NEVER a throw into render; an empty list is the honest pre-retention answer. Export from `features/transcript/index.ts`.

`swimlane/AttemptNavigator.tsx`: call `useRetainedStreams({ workflowId, namespace })`; build `const retainedKeys = useMemo(() => new Set(retained.streams.map((s) => attemptRowKey(s.activityId, s.attempt))), [retained.streams]);` (`attemptRowKey` — `lib/attemptNavigator.ts:44-46`); pass to `AttemptRowList` (`AttemptNavigator.tsx:140-173`) as `retainedKeys: ReadonlySet<string>`; each row whose key is in the set appends `<span className="text-[10px] text-muted-foreground" data-testid={`retained:${row.key}`}>· retained</span>` inside its button. Rows themselves stay timeline-derived (`deriveAttemptNavigator`, `lib/attemptNavigator.ts:102-112` — every started attempt is already durably enumerated by `ActivityStarted`; the enumeration endpoint is a badge, not a row source). This is the whole late-expansion transcript story inside an embedded child: durable rows (timeline) + retained badge (REST enumerate) + REST history + WS tail (§3.4) + intervention gating on live attempts only (`AttemptNavigator.tsx:76-89`).

---

## 4. Test plan (bun:test + `renderToStaticMarkup` SSR + pure functions — the house convention, e.g. `swimlane/Swimlane.test.tsx:1-9`, `__tests__/TranscriptPanel.test.tsx:26-77`; NO new test frameworks; reuse the existing event/`ActivityEvent` fixture builders from those files)

`swimlane/Swimlane.test.tsx` (extend; child event fixtures exist in `__tests__/components.test.tsx`'s `childStarted`):
1. `child lane carries its child workflow id for expansion` — `layoutSwimlane(projectTimeline([workflowStarted(1), childStarted(2)]))`: the `child:child-1` lane has `childWorkflowId === 'child-1'`; the lifecycle lane has `null`.
2. `swimlane renders an expand-run affordance only for child lanes` — SSR `<Swimlane entries={…child + activity…} renderChildRun={() => null} …/>`: markup contains `data-testid="expand-child:child-1"` with `aria-expanded="false"`, and NO `expand-child:` for the activity lane.
3. `an initially expanded child lane renders the injected run view beneath the chart` — `initialExpandedChildren={['child-1']}` + `renderChildRun={(id) => <div data-testid={\`stub-${id}\`}>embedded-stub</div>}`: markup contains `data-testid="child-run:child-1"`, `embedded-stub`, and `aria-expanded="true"` on the toggle.
4. `no expansion affordance renders without a renderChildRun injection` — omit the prop (+ even with `initialExpandedChildren`): no `expand-child:` / `child-run:` testids.
5. `expanded lanes nest recursively to grandchild depth` — `renderChildRun` returns a second `<Swimlane>` over grandchild-projected entries with its own stub `renderChildRun` and `initialExpandedChildren`: markup contains BOTH `child-run:child-1` and the grandchild stub (executes the recursive render path).
6. `embedded content renders the ancestry breadcrumb, status, and full-view link` — SSR `<WorkflowDetailViewContent embedded ancestry={['parent-1']} workflowId="child-1" …minimum timeline… />` wrapped in a `MemoryRouter` (react-router is already a dependency; `Link` requires a router — if the suite convention forbids routers in SSR tests, render via `createMemoryRouter`+`RouterProvider` exactly once here): contains `aria-label="Workflow ancestry"`, `/workflows/parent-1`, `data-testid="open-full-view"` with `/workflows/child-1`, `Agent attempts` (intervention surface present at depth), and NOT the `<h1>` `Workflow child-1` page header.
7. `isEmbedCycle guards ancestor re-entry` (pure) — `isEmbedCycle('a', ['a']) === true`, `isEmbedCycle('b', ['a']) === false`; plus SSR `CycleNotice`: contains `data-testid="embed-cycle"` and the id.

`lib/api/transcript-read.test.ts` (new; driver = the `client.test.ts` shape: `fetchImpl` capturing `new Request(input, init)` + `jsonResponse`, `client.test.ts:46-60`):
8. `fetchTranscript posts the stream identity with scoped headers and returns the retained events` — asserts URL `https://aion.example/workflows/transcript`, method POST, body `{ namespace, workflow_id, activity_id, attempt }` (NO `from_seq` key when unset), headers `x-aion-namespaces`/`x-aion-subject`/`authorization`, result preserves `store_seq`.
9. `fetchTranscript carries from_seq when resuming` — `fromSeq: 3` ⇒ body `from_seq: 3`.
10. `fetchTranscript surfaces a malformed envelope as an ApiError` — body `{}` ⇒ throws `ApiError` mentioning `events`.
11. `listStreams normalizes retained stream heads` — response `{ streams: [{ activity_id: 3, attempt: 0, head: 5 }] }` ⇒ `[{ activityId: 3, attempt: 0, head: 5 }]`; URL `/workflows/transcripts`.
12. `a non-OK transcript read throws the server's wire error` — 403 + error body ⇒ `ApiError` with status 403.

`lib/api/transcript-stream.test.ts` (extend; FakeSocket driver in-file, `transcript-stream.test.ts:35-59`):
13. `a manager seeded with initialAfterSeq subscribes with after_seq immediately` — `initialAfterSeq: 4` ⇒ first sent frame's `subscription.transcript.after_seq === 4` (contrast with the existing fresh-subscriber test at `:35`).

`features/transcript/__tests__/foldTranscriptEvent.test.ts` (extend):
14. `backfillEntries folds a retained transcript in order and reports the resume cursor` — three persisted events (seqs 0,1,2, one a Note run candidate) ⇒ ordered entries, `lastSeq === 2`.
15. `backfillEntries on an empty retained transcript reports no cursor` — `{ entries: [], lastSeq: undefined }` (pre-retention run ⇒ WS full replay).

`features/transcript/__tests__/TranscriptPanel.test.tsx` (extend):
16. `surfaces a retained-backfill failure as visible state` — `TranscriptPanelContent` with `backfillError: new Error('boom')` ⇒ markup contains `data-testid="transcript-backfill-error"` and `boom`.

`features/workflow-detail/__tests__/` (AttemptNavigator badge — render the presentational `AttemptRowList` if exported, else SSR `AttemptNavigator` is NOT possible (hooks fetch); export `AttemptRowList` for the test):
17. `attempt rows badge retained transcripts from the enumeration` — rows + `retainedKeys` containing one key ⇒ `data-testid="retained:3:1"` present exactly for that row.

Regression: the full existing console suite must stay green (notably `Swimlane.test.tsx`'s Content imports via `./WorkflowDetailView` — §1.2 re-export keeps them compiling).

---

## 5. Workspace laws (apply throughout)
No `unwrap`/`expect`/`panic` (no Rust is expected to change; the law still binds anything touched); no `#[allow]`/`#[expect]`/`#[ignore]`; files ≤500 CODE lines (split per §1.1/§3.4 notes; do NOT grow `client.ts`); `mod.rs`/`index.ts` re-exports only; backticked identifiers in doc comments. Console: biome + tsc are the law-enforcers; keep named exports (CO4 — `app/routes.tsx:223-229` comment). Formatters: `cargo fmt` (workspace) and `biome format --write .` in `apps/aion-ops-console` — never format-CHECK commands.

## 6. Gates (redirect FULL output to `<worktree>/t230-gates/*.log`; never pipe through grep/tail/head; record every exit code in `t230-gates/manifest.txt`)
1. `bun install` in `<worktree>/apps/aion-ops-console` → `bun-install.log`.
2. `bun test` in `apps/aion-ops-console` → `test-console.log` (all new + existing tests).
3. `bun run typecheck` (`tsc --noEmit`) → `typecheck.log`.
4. `biome format --write .` in `apps/aion-ops-console`, then `bun run lint` → `lint.log` (biome lint is a check-style gate the repo scripts define — `package.json:12`; run it, fix findings, re-run).
5. `cargo fmt` at the worktree root (no-op expected).
6. **MANDATORY EMBED REGEN**: `CARGO_TARGET_DIR=/Users/tom/Developer/ablative/aion/target cargo xtask build-ops-console` → `embed-regen.log` (pipeline: ts-rs regen + bun build + sync into `crates/aion-server/ops-console-embed/` — `xtask/src/main.rs:62-86,151-191`). The regenerated bundle MUST be committed (a stale embed already caused a production incident). If the ts-rs step rewrites `src/types/generated/`, commit that too (expected no-op — no Rust type changed).
7. `CARGO_TARGET_DIR=… cargo xtask verify-ops-console` → `embed-verify.log` (byte-for-byte freshness proof — `xtask/src/main.rs:89+`).
8. `CARGO_TARGET_DIR=… cargo build -p aion-server` → `build-server.log` (the committed embed compiles into the binary).

Do NOT push, merge, touch main, deploy, or restart anything — a production server is live on this machine.

## 7. Commit
On `lane/t230-nested-child-lanes`, staging EXPLICIT paths only: the touched files under `apps/aion-ops-console/src/`, `apps/aion-ops-console/package.json`/lockfile only if bun changed them, and `crates/aion-server/ops-console-embed/` (regenerated). Message ends with the trailer line:
`Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`

## 8. Ordered checklist
1. Create worktree/branch off `lane/t229-transcript-retention` (§ header).
2. A: extract `WorkflowDetailViewContent.tsx` (verbatim move) + re-exports; suite still green (`bun test`) before any behavior change.
3. A: add `embedded`/`ancestry`/`renderChildRun` props + embedded header (§1.1); `EmbeddedRunView.tsx` with `isEmbedCycle`/`CycleNotice` (§1.3); wire `renderChildRun` in `WorkflowDetailView.tsx` (§1.2); exports (§1.4).
4. B: `laneLayout.ts` `SwimlaneLane.childWorkflowId` (§2.1); `Swimlane.tsx` expand state + toggle + beneath-chart child-run regions (§2.2).
5. C: `client-transport.ts` `buildScopedHeaders` extraction + `client.ts` refactor; `transcript-read.ts` (§3.1); `clients.ts` reader factory (§3.2).
6. C: `transcript-stream.ts` `initialAfterSeq` + configured-stream passthrough (§3.3).
7. C: `useTranscript` backfill rework + `backfillEntries` + panel error surface (§3.4).
8. C: `useRetainedStreams` + AttemptNavigator badge (§3.5).
9. Tests 1–17 (§4); run gates 1–4 fixing failures.
10. Gates 5–8; write `t230-gates/manifest.txt` (every gate → exit code, all zero).
11. Commit per §7 (embed bundle included). No push, no merge.
