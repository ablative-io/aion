import type { ApiClient } from '@/lib/api';
import type { Namespace } from '@/types';

import type { DeployClient } from '../hooks/useDeployPackage';
import { DeployPackagePanel } from './DeployPackagePanel';
import { StartWorkflowForm } from './StartWorkflowForm';

export type ActionsViewProps = {
  namespace: Namespace | null;
  /** Injected start client (tests). */
  startClient?: Pick<ApiClient, 'startWorkflow'> | undefined;
  /** Injected deploy client (tests). */
  deployClient?: DeployClient | undefined;
  /**
   * Whether the resolved caller may deploy, discovered at runtime from
   * `GET /whoami` (NOT a build-time flag). When `false` the deploy affordance is
   * not rendered at all — the console only ever shows what the server grants
   * this caller. Defaults to `false`: an ungated render is unprivileged.
   */
  deployGranted?: boolean;
};

/**
 * Operator actions: start a workflow run (namespace-scoped command authority)
 * and deploy a package (deployment-scoped deploy grant). The two live side by
 * side but are distinct authorities (ADR-022), and each renders its own honest
 * loading / error / confirmed-success state.
 *
 * The deploy affordance is gated on the runtime-discovered `deployGranted`
 * capability, so it appears only for a caller the server actually authorizes to
 * deploy (the single-tenant operator, or a deploy-claim bearer under real auth).
 */
export function ActionsView({
  namespace,
  startClient,
  deployClient,
  deployGranted = false,
}: ActionsViewProps) {
  return (
    <div className="space-y-8 py-4">
      <header>
        <h1 className="font-medium text-foreground text-xl">Actions</h1>
        <p className="text-muted-foreground text-sm">
          Start a workflow run or deploy a package. Success is shown only after the server confirms.
        </p>
      </header>

      <section className="space-y-3" aria-label="Start workflow">
        <h2 className="font-medium text-secondary-foreground text-sm uppercase tracking-[0.15em]">
          Start workflow
        </h2>
        <StartWorkflowForm apiClient={startClient} namespace={namespace} />
      </section>

      {deployGranted ? (
        <section className="space-y-3" aria-label="Deploy package">
          <h2 className="font-medium text-secondary-foreground text-sm uppercase tracking-[0.15em]">
            Deploy package
          </h2>
          <DeployPackagePanel apiClient={deployClient} />
        </section>
      ) : null}
    </div>
  );
}
