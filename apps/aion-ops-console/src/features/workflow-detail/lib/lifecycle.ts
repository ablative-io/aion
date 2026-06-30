import type { Event } from '@/types';

import type { LifecycleOutcome } from '../types';
import { payloadSummary } from './payload';

export type LifecycleType =
  | 'WorkflowStarted'
  | 'WorkflowCompleted'
  | 'WorkflowFailed'
  | 'WorkflowCancelled'
  | 'WorkflowTimedOut';

export type TerminalLifecycleType = Exclude<LifecycleType, 'WorkflowStarted'>;

const LIFECYCLE_OUTCOMES: Record<LifecycleType, LifecycleOutcome> = {
  WorkflowStarted: 'started',
  WorkflowCompleted: 'completed',
  WorkflowFailed: 'failed',
  WorkflowCancelled: 'cancelled',
  WorkflowTimedOut: 'timed-out',
};

export function lifecycleOutcome(event: Extract<Event, { type: LifecycleType }>): LifecycleOutcome {
  return LIFECYCLE_OUTCOMES[event.type];
}

export function lifecycleSummary(event: Extract<Event, { type: LifecycleType }>): string {
  switch (event.type) {
    case 'WorkflowStarted':
      return `Workflow started: ${event.data.workflow_type}`;
    case 'WorkflowCompleted':
      return `Workflow completed with ${payloadSummary(event.data.result)}`;
    case 'WorkflowFailed':
      return `Workflow failed: ${event.data.error.message}`;
    case 'WorkflowCancelled':
      return `Workflow cancelled: ${event.data.reason}`;
    case 'WorkflowTimedOut':
      return `Workflow timed out after ${event.data.timeout}`;
    default:
      return assertNeverLifecycle(event);
  }
}

export function lifecyclePayload(event: Extract<Event, { type: LifecycleType }>): unknown {
  switch (event.type) {
    case 'WorkflowStarted':
      return event.data.input;
    case 'WorkflowCompleted':
      return event.data.result;
    case 'WorkflowFailed':
      return event.data.error;
    case 'WorkflowCancelled':
      return { reason: event.data.reason };
    case 'WorkflowTimedOut':
      return { timeout: event.data.timeout };
    default:
      return assertNeverLifecycle(event);
  }
}

function assertNeverLifecycle(value: never): never {
  throw new Error(`Unhandled lifecycle event variant: ${JSON.stringify(value)}`);
}
