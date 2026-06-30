# aion-dashboard Completeness Audit

Synthesized, deduped, severity-ordered defect list from a multi-pass completeness audit
(reachability, honesty/no-silent-failure, shortcuts/type-escapes, house-rules/VISION).

Overall completeness is high: routing/navigation is fully wired, honesty and no-silent-failure
discipline is exemplary (no fabricated data, no swallowed errors, every value tied to a real
backing signal), no skipped/trivial tests, no TODO/FIXME in production code, all files within
size limits. Remaining defects fall into three buckets: (1) two abandoned/unreachable component
trees that violate the zero-dead-code phase-1 bar, (2) a cluster of VISION §1 "hand-plane
principle" violations (semi-transparent colors + a glow effect) across status/error styling,
and (3) a handful of localized type-escapes (mostly in tests) and lint suppressions.

False positives dropped: input `placeholder=` props are not defects; the documented
server-gated "Awaiting server support" affordances are correct honesty behavior, not defects;
the reachability index.ts:3 export note is folded into the WorkflowDetail dead-code defect (same
file, same remediation) rather than counted separately.

## Blockers

_None._

## Major

### Reachability

| file:line | severity | category | problem | fix |
|---|---|---|---|---|
| src/features/workflow-detail/components/WorkflowDetail.tsx:29,54 (and index.ts:3) | major | reachability | `WorkflowDetail` + `WorkflowDetailContent` are exported from the feature index but never instantiated anywhere; `WorkflowDetailView`/`WorkflowDetailViewContent` (swimlane) supersede them as the route's detail renderer. Well-written but unreachable dead code; violates the zero-dead-code phase-1 bar. | Remove `components/WorkflowDetail.tsx` and delete the `export { WorkflowDetail, WorkflowDetailContent }` line at `index.ts:3` (preferred — superseded). If an intentional alternate view is wanted, mount it from a route/panel instead. |
| src/features/workflow-detail/reopen/ReopenDiff.tsx:44 | major | reachability | `ReopenDiff` is fully orphaned: exported from `reopen/index.ts`, not re-exported from the parent feature `index.ts`, never imported, no route mounts it. Feature-complete with tests but unwired. | Remove the entire `/reopen` directory (`ReopenDiff.tsx`, `computeReopen.ts`, `computeReopen.test.ts`, `index.ts`) as dead code, or wire it into a detail-view panel/modal if it is a planned reopen UI. |

### House rule (VISION §1 hand-plane principle: no glass / semi-transparent overlays / chrome)

| file:line | severity | category | problem | fix |
|---|---|---|---|---|
| src/features/failover/components/NodeCard.tsx:26 | major | houserule | `shadow-[0_0_8px_var(--accent-cyan-glow)]` adds a glow ("chrome") around the live liveness dot. | Remove the `shadow-[...]` glow; convey "live" via solid color/border alone. |
| src/features/workflow-detail/swimlane/LaneBar.tsx:80-86 | major | houserule | `STATUS_CLASSES` uses semi-transparent backgrounds (`bg-sky-500/20`, `bg-emerald-500/20`, `bg-red-500/25`, etc.) — forbidden glass/transparency. | Replace opacity-modded colors with solid, opaque status colors; distinguish status by shape + solid color. |
| src/features/live-feed/components/ConnectionIndicator.tsx:1-50 | major | houserule | Status indicators use opacity modifiers throughout (`bg-emerald-500/10`, `bg-destructive/10`, `bg-amber-500/10`, `border-*/40`). | Replace all `X/Y` opacity classes with solid, opaque background/border colors. |
| src/features/live-feed/components/FirehoseFeed.tsx (various) | major | houserule | Error/warning states use semi-transparent backgrounds/borders (`bg-destructive/10`, `bg-amber-500/10`, `border-destructive/40`, `border-amber-500/30`). | Replace all opacity-modded colors with solid, opaque alternatives. |
| src/features/failover/components/ExactlyOnceCounter.tsx (various) | major | houserule | Counter status badges use `border-amber-500/40`, `bg-amber-500/10`, `border-destructive/40`, `bg-destructive/10`. | Replace with solid, opaque badge colors conveying status by color + shape. |
| src/features/failover/components/FanOutBar.tsx (various) | major | houserule | Progress/error states use `border-destructive/40`, `bg-destructive/10`. | Replace opacity-modded colors with solid, opaque background/border colors. |
| src/features/failover/FailoverView.tsx:158 | major | houserule | Error container uses `bg-[var(--destructive)]/5` + `border-[var(--destructive)]/40`. | Replace semi-transparent error styling with solid, opaque colors. |
| src/features/failover/components/NodeCard.tsx:65 | major | houserule | Dark node card uses `border-[var(--destructive)]/40` + `bg-[var(--destructive)]/5`. | Replace opacity-modded colors with solid, opaque colors while keeping the dark-state distinction. |
| src/features/workflow-detail/components/ActivityGroup.tsx (various) | major | houserule | Failure activity details rendered with `bg-red-500/10`. | Replace `bg-red-500/10` with a solid, opaque background that preserves hierarchy. |
| src/components/ErrorState.tsx:13 | major | houserule | Error container uses `border-[var(--destructive)]/40` + `bg-[var(--destructive)]/5`. | Replace semi-transparent error styling with solid, opaque background/border. |
| src/components/StatusBadge.tsx (various) | major | houserule | Status variants use `border-sky-400/30`, `bg-sky-500/15`, etc. | Replace all opacity-modded badge colors with solid, opaque alternatives. |

## Minor

### Shortcut / type escape

| file:line | severity | category | problem | fix |
|---|---|---|---|---|
| src/features/failover/FailoverView.tsx:198 | minor | shortcut | `event as never` works around type narrowing in a loop over `{ type: string }` events passed to `eventSequence`, bypassing compile-time verification. | Narrow the event type explicitly before calling, or widen the type guard to match the function's parameter type, removing the cast. |
| src/features/failover/hooks/useFanOutProgress.test.ts:193 | minor | shortcut | `CapturingSocket as unknown as never` forces a fake socket into the `WebSocketConstructor` signature, hiding fake-impl mismatches. | Have `CapturingSocket` properly implement `WebSocketConstructor` (required handlers/props) so the cast is unnecessary. |
| src/features/search/__tests__/SearchView.test.tsx:27 | minor | shortcut | `as unknown as Event` coerces an `Activity` fixture into `Event`, bypassing strict checking of the fixture shape. | Build the fixture with a proper `Event` shape (all required `data` fields) or a type-safe `Event` factory. |
| src/app/AppShell.test.tsx:143 | minor | shortcut | `(value as { type?: unknown }).type` accesses an optional field without narrowing, bypassing strict null checks. | Use a type-guard to narrow `value` before reading `type`, or optional chaining with null coalescing. |
| src/features/failover/FailoverFallback.tsx:93 | minor | shortcut | `biome-ignore noNonNullAssertion` on `workflowId!` relies on the `enabled` guard, a correlation TS cannot verify in all contexts. | Refactor to remove the assertion: extract query logic into a helper that returns null when `workflowId` is null. |
| src/features/failover/components/FanOutBar.tsx:47 | minor | shortcut | `biome-ignore noArrayIndexKey` suppresses array-index-as-key; the rule exists to catch dynamic-key bugs even on "fixed-length" lists. | Render segments via a sub-component keyed by a stable id/value, or add a unique id per segment, instead of the index. |

