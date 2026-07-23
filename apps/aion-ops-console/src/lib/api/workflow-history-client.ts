import type { Event, WorkflowId } from '@/types';

import type { ApiRequestTransport } from './client-request';
import { AW_REST_CONTRACT } from './client-transport';
import type { RequestOptions } from './client-types';
import {
  decodeEventResponse,
  decodeHistoryWindow,
  type HistoryWindow,
  type HistoryWindowRequest,
} from './workflow-history-contract';

export async function requestHistoryWindow(
  transport: ApiRequestTransport,
  workflowId: WorkflowId,
  window: HistoryWindowRequest,
  options: RequestOptions
): Promise<HistoryWindow> {
  const response = await transport.request<unknown>(
    AW_REST_CONTRACT.endpoints.historyWindow,
    AW_REST_CONTRACT.methods.historyWindow,
    options,
    {
      namespace: options.namespace,
      workflow_id: workflowId,
      ...(window.fromSeq === undefined ? {} : { from_seq: window.fromSeq }),
      ...(window.limit === undefined ? {} : { limit: window.limit }),
      ...(window.payloadLimitBytes === undefined
        ? {}
        : { payload_limit_bytes: window.payloadLimitBytes }),
    }
  );
  return decodeHistoryWindow(response);
}

export async function requestWorkflowEvent(
  transport: ApiRequestTransport,
  workflowId: WorkflowId,
  seq: number,
  options: RequestOptions
): Promise<Event> {
  const response = await transport.request<unknown>(
    AW_REST_CONTRACT.endpoints.workflowEvent,
    AW_REST_CONTRACT.methods.workflowEvent,
    options,
    { namespace: options.namespace, workflow_id: workflowId, seq }
  );
  return decodeEventResponse(response);
}
