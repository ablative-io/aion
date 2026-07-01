import { describe, expect, test } from 'bun:test';

import type { ActivityId, Namespace, WorkflowId } from '@/types';
import type { TranscriptTarget } from '@/lib/api/transcript-stream';
import { parseTargetKey, targetKey } from '../hooks/useTranscript';

/**
 * Guards the `targetKey` <-> `parseTargetKey` round-trip. This pair serializes a
 * transcript target into a stable string for the effect dependency, then recovers
 * it inside the effect. A join/split delimiter mismatch here silently corrupts the
 * recovered target (blank namespace / NaN attempt), so the socket subscribes to a
 * nonexistent target and the panel shows an empty transcript forever — a failure
 * that leaves every other test green. The delimiter is NUL, which can never appear
 * in these identifier fields, so a namespace or workflow id that contains a space
 * must still round-trip exactly.
 */
describe('transcript target key round-trip', () => {
  test('round-trips a normal target', () => {
    const target: TranscriptTarget = {
      namespace: 'default' as Namespace,
      workflowId: 'wf-0192' as WorkflowId,
      activityId: 7 as ActivityId,
      attempt: 2,
    };
    expect(parseTargetKey(targetKey(target))).toEqual(target);
  });

  test('round-trips fields containing a space (NUL-delimiter robustness)', () => {
    // A space-delimited join would mis-split these; the NUL delimiter must not.
    const target: TranscriptTarget = {
      namespace: 'team alpha' as Namespace,
      workflowId: 'order saga 9' as WorkflowId,
      activityId: 3 as ActivityId,
      attempt: 1,
    };
    expect(parseTargetKey(targetKey(target))).toEqual(target);
  });
});
