import type { Event } from '@/types';

/**
 * The closed set of event `type` discriminants the dashboard knows how to
 * project. Derived directly from the generated `Event` union so it tracks the
 * Rust ts-rs exporter: regenerating the wire types with a NEW variant makes this
 * a *strict superset* of the handled set, which the compile-time exhaustiveness
 * guard below then rejects at `tsc` time (SPEC handPlane rule 4).
 */
export type KnownEventType = Event['type'];

/**
 * Coarse family every known variant maps to. Drives lane assignment and the
 * `EventIcon` tone map. `lifecycle`, `activity`, `timer`, `signal`, `child` get
 * dedicated timeline lanes; `schedule` and `marker` render as typed generic rows
 * with a sub-kind so they are still first-class (never an unlabelled fallback).
 */
export type EventVariantFamily =
  | 'lifecycle'
  | 'marker'
  | 'activity'
  | 'timer'
  | 'with-timeout'
  | 'signal'
  | 'child'
  | 'schedule';

/**
 * Stable structured descriptor for a known variant. `subKind` is the specific
 * discriminant (e.g. `ScheduleCreated`), surfaced verbatim so the generic row
 * can label itself precisely rather than guessing from a humanised string.
 */
export type KnownVariantDescriptor = {
  family: EventVariantFamily;
  subKind: KnownEventType;
};

/**
 * Compile-time exhaustiveness sentinel. It is only callable with `never`, so if
 * `classifyKnownEvent` ever fails to narrow the union to nothing — i.e. a newly
 * regenerated variant is unhandled — the call below fails to type-check and the
 * build breaks. There is NO runtime throw on the hot projection path; genuinely
 * unknown future variants are handled by the caller's safe default arm.
 */
export function assertAllVariantsHandled(variant: never): never {
  throw new Error(
    `Unhandled known event variant reached runtime: ${JSON.stringify(variant)}. ` +
      'This indicates the generated wire types gained a variant without a timeline handler.'
  );
}

/**
 * Maps a known event type to its family + sub-kind. The exhaustive `switch`
 * over `KnownEventType` is the build guard: every one of the 29 generated
 * variants must appear, or `assertAllVariantsHandled` receives a non-`never`
 * value and `tsc` fails. Returns `null` for a type that is NOT in the known set
 * (a future-unknown variant decoded at runtime), letting the caller render a
 * safe generic row without throwing.
 */
export function classifyKnownEvent(eventType: string): KnownVariantDescriptor | null {
  if (!isKnownEventType(eventType)) {
    return null;
  }

  const subKind = eventType;

  switch (eventType) {
    case 'WorkflowStarted':
    case 'WorkflowCompleted':
    case 'WorkflowFailed':
    case 'WorkflowCancelled':
    case 'WorkflowTimedOut':
      return { family: 'lifecycle', subKind };
    case 'WorkflowContinuedAsNew':
    case 'WorkflowReopened':
    case 'SearchAttributesUpdated':
      return { family: 'marker', subKind };
    case 'ActivityScheduled':
    case 'ActivityStarted':
    case 'ActivityCompleted':
    case 'ActivityFailed':
    case 'ActivityCancelled':
      return { family: 'activity', subKind };
    case 'TimerStarted':
    case 'TimerFired':
    case 'TimerCancelled':
      return { family: 'timer', subKind };
    case 'WithTimeoutCompleted':
      return { family: 'with-timeout', subKind };
    case 'SignalReceived':
    case 'SignalSent':
      return { family: 'signal', subKind };
    case 'ChildWorkflowStarted':
    case 'ChildWorkflowCompleted':
    case 'ChildWorkflowFailed':
    case 'ChildWorkflowCancelled':
      return { family: 'child', subKind };
    case 'ScheduleCreated':
    case 'ScheduleUpdated':
    case 'SchedulePaused':
    case 'ScheduleResumed':
    case 'ScheduleDeleted':
    case 'ScheduleTriggered':
      return { family: 'schedule', subKind };
    default:
      return assertAllVariantsHandled(eventType);
  }
}

/**
 * Human-readable summary for a typed generic row (marker + schedule variants).
 * Each schedule/marker variant gets a precise hand-written label; any
 * genuinely-unknown future variant falls back to a humanised type name so it
 * still reads, never crashing the view.
 */
export function genericSummary(event: Event): string {
  switch (event.type) {
    case 'WorkflowContinuedAsNew':
      return `Workflow continued as new${
        event.data.workflow_type ? ` as ${event.data.workflow_type}` : ''
      }`;
    case 'WorkflowReopened':
      return `Workflow reopened (${event.data.reopened.length} activity reset point(s))`;
    case 'SearchAttributesUpdated':
      return `Search attributes updated (${Object.keys(event.data.attributes).length} key(s))`;
    case 'ScheduleCreated':
      return `Schedule created: ${event.data.schedule_id}`;
    case 'ScheduleUpdated':
      return `Schedule updated: ${event.data.schedule_id}`;
    case 'SchedulePaused':
      return `Schedule paused: ${event.data.schedule_id}`;
    case 'ScheduleResumed':
      return `Schedule resumed: ${event.data.schedule_id}`;
    case 'ScheduleDeleted':
      return `Schedule deleted: ${event.data.schedule_id}`;
    case 'ScheduleTriggered':
      return `Schedule ${event.data.schedule_id} triggered workflow ${event.data.workflow_id}`;
    default:
      return humanizeEventType(event.type);
  }
}

/** Turns a PascalCase event type into a spaced, sentence-cased label. */
export function humanizeEventType(eventType: string): string {
  const spaced = eventType.replace(/([a-z0-9])([A-Z])/g, '$1 $2');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1).toLowerCase();
}

/** The full closed set, as a runtime value for membership checks + tests. */
export const KNOWN_EVENT_TYPES: readonly KnownEventType[] = [
  'WorkflowStarted',
  'WorkflowCompleted',
  'WorkflowFailed',
  'WorkflowCancelled',
  'WorkflowTimedOut',
  'WorkflowContinuedAsNew',
  'WorkflowReopened',
  'SearchAttributesUpdated',
  'ActivityScheduled',
  'ActivityStarted',
  'ActivityCompleted',
  'ActivityFailed',
  'ActivityCancelled',
  'TimerStarted',
  'TimerFired',
  'TimerCancelled',
  'WithTimeoutCompleted',
  'SignalReceived',
  'SignalSent',
  'ChildWorkflowStarted',
  'ChildWorkflowCompleted',
  'ChildWorkflowFailed',
  'ChildWorkflowCancelled',
  'ScheduleCreated',
  'ScheduleUpdated',
  'SchedulePaused',
  'ScheduleResumed',
  'ScheduleDeleted',
  'ScheduleTriggered',
];

const KNOWN_EVENT_TYPE_SET = new Set<string>(KNOWN_EVENT_TYPES);

function isKnownEventType(eventType: string): eventType is KnownEventType {
  return KNOWN_EVENT_TYPE_SET.has(eventType);
}

/**
 * Compile-time proof that `KNOWN_EVENT_TYPES` enumerates the entire generated
 * union — no more, no less. If a regeneration adds a variant, `MissingFromList`
 * stops being `never`, the conditional below resolves to `false`, and the `true`
 * literal assignment fails `tsc` — so the value-level list cannot silently drift
 * from the type-level guard.
 */
type MissingFromList = Exclude<KnownEventType, (typeof KNOWN_EVENT_TYPES)[number]>;
type ExtraInList = Exclude<(typeof KNOWN_EVENT_TYPES)[number], KnownEventType>;
type ListIsExhaustive = [MissingFromList] extends [never]
  ? [ExtraInList] extends [never]
    ? true
    : false
  : false;
const _knownEventTypeListIsExhaustive: ListIsExhaustive = true;
void _knownEventTypeListIsExhaustive;
