import { createDraftStore } from '@/lib/state';

/** Raw string state of the start form's inputs (always strings, possibly blank). */
export type StartWorkflowDraft = {
  workflowType: string;
  namespaceEntry: string;
  inputText: string;
  routingKey: string;
  taskQueue: string;
};

export const EMPTY_START_WORKFLOW_DRAFT: StartWorkflowDraft = {
  workflowType: '',
  namespaceEntry: '',
  inputText: '',
  routingKey: '',
  taskQueue: '',
};

/** A half-filled start form survives navigating away; cleared on a confirmed start. */
export const startWorkflowDraftStore = createDraftStore({
  name: 'start-workflow',
  empty: EMPTY_START_WORKFLOW_DRAFT,
});
