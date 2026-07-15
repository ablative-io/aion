import type { WorkflowId } from '@/types';

import type { TimelineEntry } from '../types';
import type { SwimlaneSelection } from './Swimlane';

export type SelectionSurface = {
  workflowId: WorkflowId;
  timeline: readonly TimelineEntry[];
  entry: TimelineEntry | null;
  clusterEntries: readonly TimelineEntry[];
  scopedSelection: SwimlaneSelection | null;
};

/** Resolve every workflow-id-driven surface from the exact bar click scope. */
export function resolveSelectionSurface(
  rootWorkflowId: WorkflowId,
  rootTimeline: readonly TimelineEntry[],
  selectedSequence: number | null,
  scopedSelection: SwimlaneSelection | null
): SelectionSurface {
  const activeScopedSelection =
    scopedSelection?.sequence === selectedSequence ? scopedSelection : null;
  const timeline = activeScopedSelection?.timeline ?? rootTimeline;
  const workflowId = activeScopedSelection?.workflowId ?? rootWorkflowId;
  const entry = timeline.find((candidate) => candidate.sequence === selectedSequence) ?? null;
  const clusterEntries =
    activeScopedSelection === null
      ? []
      : activeScopedSelection.clusterSequences.flatMap((sequence) => {
          const member = timeline.find((candidate) => candidate.sequence === sequence);
          return member === undefined ? [] : [member];
        });

  return {
    workflowId,
    timeline,
    entry,
    clusterEntries,
    scopedSelection: activeScopedSelection,
  };
}
