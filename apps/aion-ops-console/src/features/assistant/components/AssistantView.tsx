import type { ApiClient } from '@/lib/api';
import type { Namespace } from '@/types';

import { AssistantSessionList } from './AssistantSessionList';
import { NewAssistantSessionForm } from './NewAssistantSessionForm';

export type AssistantViewProps = {
  namespace: Namespace | null;
  /** Injected client (tests); defaults to the configured live client. */
  client?: (Pick<ApiClient, 'queryWorkflows'> & Pick<ApiClient, 'startWorkflow'>) | undefined;
};

/**
 * The assistant panel root (`/assistant`): start a new session + the live
 * session list. v1 is deliberately plain — the session view carries the work.
 */
export function AssistantView({ namespace, client }: AssistantViewProps) {
  return (
    <section className="space-y-6">
      <header className="space-y-1">
        <h1 className="font-semibold text-foreground text-lg">Assistant</h1>
        <p className="text-muted-foreground text-sm">
          Round-based agent sessions on the assistant workflow — start one, watch it work, steer it,
          continue or end it.
        </p>
      </header>
      <NewAssistantSessionForm apiClient={client} namespace={namespace} />
      <AssistantSessionList client={client} namespace={namespace} />
    </section>
  );
}
