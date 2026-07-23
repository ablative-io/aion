import type { TimelineEntry } from '../types';
import { layoutSwimlane, type SwimlaneLane } from './laneLayout';
import type { VisibleWorkflow } from './timeLayout';

export type ChildTimelineState =
  | { status: 'loading'; entries: readonly TimelineEntry[]; isRunning: true }
  | { status: 'error'; entries: readonly TimelineEntry[]; isRunning: false; message: string }
  | { status: 'ready'; entries: readonly TimelineEntry[]; isRunning: boolean };

export type WorkflowTreeNode = {
  path: string;
  workflowId: string;
  ancestry: readonly string[];
  depth: number;
  entries: readonly TimelineEntry[];
  isRunning: boolean;
};

export type LaneTreeRow =
  | {
      kind: 'lane';
      id: string;
      path: string;
      workflowId: string;
      depth: number;
      lane: SwimlaneLane;
      /** Non-null on the timer aggregate, or its first expanded member. */
      timerGroupExpanded: boolean | null;
    }
  | {
      kind: 'notice';
      id: string;
      path: string;
      workflowId: string;
      depth: number;
      notice: 'loading' | 'error' | 'empty' | 'cycle';
      message: string;
    };

export type ChildTimelineRequest = {
  path: string;
  workflowId: string;
  ancestry: readonly string[];
};

export type LaneTree = {
  rows: LaneTreeRow[];
  workflows: VisibleWorkflow[];
  requests: ChildTimelineRequest[];
};

export function childNodePath(parentPath: string, childWorkflowId: string): string {
  return `${parentPath}>${childWorkflowId}`;
}

export function timerGroupPath(workflowPath: string): string {
  return `${workflowPath}:timer-group`;
}

/** An id already in the branch's ancestry must never be fetched or recursed into. */
export function isWorkflowCycle(workflowId: string, ancestry: readonly string[]): boolean {
  return ancestry.includes(workflowId);
}

/**
 * Flatten expanded workflow lanes into depth-indented rows. The tree only decides
 * row order; every row still uses the one shared track origin and axis in the view.
 */
export function flattenLaneTree(
  root: Omit<WorkflowTreeNode, 'path' | 'ancestry' | 'depth'>,
  children: ReadonlyMap<string, ChildTimelineState>,
  expandedPaths: ReadonlySet<string>,
  initiallyExpandedChildIds: ReadonlySet<string> = new Set(),
  collapsedPaths: ReadonlySet<string> = new Set()
): LaneTree {
  const rows: LaneTreeRow[] = [];
  const workflows: VisibleWorkflow[] = [];
  const requests: ChildTimelineRequest[] = [];

  visit({ ...root, path: root.workflowId, ancestry: [], depth: 0 });

  return { rows, workflows, requests };

  function visit(node: WorkflowTreeNode): void {
    workflows.push({
      workflowId: node.workflowId,
      entries: node.entries,
      isRunning: node.isRunning,
    });
    const timersExpanded = expandedPaths.has(timerGroupPath(node.path));
    const layout = layoutSwimlane(node.entries, { expandTimers: timersExpanded });

    if (layout.lanes.length === 0 && node.depth > 0) {
      rows.push({
        kind: 'notice',
        id: `${node.path}:empty`,
        path: node.path,
        workflowId: node.workflowId,
        depth: node.depth,
        notice: 'empty',
        message: 'No correlated child events yet.',
      });
      return;
    }

    let hasTimerGroupToggle = false;
    for (const lane of layout.lanes) {
      rows.push({
        kind: 'lane',
        id: `${node.path}:${lane.id}`,
        path: node.path,
        workflowId: node.workflowId,
        depth: node.depth,
        lane,
        timerGroupExpanded: lane.kind === 'timer' && !hasTimerGroupToggle ? timersExpanded : null,
      });
      if (lane.kind === 'timer') {
        hasTimerGroupToggle = true;
      }

      const childWorkflowId = lane.childWorkflowId;
      if (childWorkflowId === null) {
        continue;
      }
      const childPath = childNodePath(node.path, childWorkflowId);
      const expanded =
        !collapsedPaths.has(childPath) &&
        (expandedPaths.has(childPath) || initiallyExpandedChildIds.has(childWorkflowId));
      if (!expanded) {
        continue;
      }

      const ancestry = [...node.ancestry, node.workflowId];
      if (isWorkflowCycle(childWorkflowId, ancestry)) {
        rows.push({
          kind: 'notice',
          id: `${childPath}:cycle`,
          path: childPath,
          workflowId: childWorkflowId,
          depth: node.depth + 1,
          notice: 'cycle',
          message: `Workflow ${childWorkflowId} is already expanded above.`,
        });
        continue;
      }

      requests.push({ path: childPath, workflowId: childWorkflowId, ancestry });
      const child = children.get(childPath);
      if (child === undefined || child.status === 'loading') {
        rows.push({
          kind: 'notice',
          id: `${childPath}:loading`,
          path: childPath,
          workflowId: childWorkflowId,
          depth: node.depth + 1,
          notice: 'loading',
          message: `Loading ${childWorkflowId} history…`,
        });
        continue;
      }
      if (child.status === 'error') {
        rows.push({
          kind: 'notice',
          id: `${childPath}:error`,
          path: childPath,
          workflowId: childWorkflowId,
          depth: node.depth + 1,
          notice: 'error',
          message: child.message,
        });
        continue;
      }

      visit({
        path: childPath,
        workflowId: childWorkflowId,
        ancestry,
        depth: node.depth + 1,
        entries: child.entries,
        isRunning: child.isRunning,
      });
    }
  }
}
