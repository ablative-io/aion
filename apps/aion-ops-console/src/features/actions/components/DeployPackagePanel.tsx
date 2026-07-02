import { useId, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { Button } from '@/components/ui';
import { ApiError, type LoadPackageResult, type WorkflowVersion } from '@/lib/api';

import {
  type DeployClient,
  useDeployPackage,
  useWorkflowVersions,
} from '../hooks/useDeployPackage';

export type DeployPackagePanelProps = {
  /** Injected client for tests. */
  apiClient?: DeployClient | undefined;
};

/**
 * Deploy a `.aion` package and view the loaded versions. Deployment-scoped
 * (deploy grant, no namespace). The upload reads the chosen file as an
 * `ArrayBuffer` and posts the raw bytes. A cluster running with
 * `[deploy] enabled=false` answers 404 — rendered as an explicit "deploy
 * disabled" state, never as a fake-empty success.
 */
export function DeployPackagePanel({ apiClient }: DeployPackagePanelProps) {
  const fileInputId = useId();
  const [selectedName, setSelectedName] = useState<string | null>(null);
  const [archive, setArchive] = useState<ArrayBuffer | null>(null);
  const [readError, setReadError] = useState<string | null>(null);

  const versions = useWorkflowVersions({ apiClient });
  const deploy = useDeployPackage({ apiClient });

  async function handleFileChange(event: React.ChangeEvent<HTMLInputElement>) {
    setReadError(null);
    deploy.reset();
    const file = event.currentTarget.files?.[0] ?? null;

    if (file === null) {
      setSelectedName(null);
      setArchive(null);
      return;
    }

    setSelectedName(file.name);
    try {
      setArchive(await file.arrayBuffer());
    } catch {
      setArchive(null);
      setReadError('Could not read the selected file.');
    }
  }

  function handleUpload() {
    if (archive === null) {
      return;
    }

    deploy.mutate({ archive });
  }

  const canUpload = archive !== null && !deploy.isPending;

  return (
    <div className="space-y-6">
      <div className="space-y-3 rounded-lg border border-border p-4">
        <label className="flex flex-col gap-2 font-medium text-sm" htmlFor={fileInputId}>
          Package archive (.aion)
          <input
            accept=".aion,application/octet-stream"
            className="text-secondary-foreground text-sm file:mr-3 file:rounded-md file:border file:border-border file:bg-transparent file:px-3 file:py-1.5 file:text-foreground file:text-sm"
            id={fileInputId}
            type="file"
            onChange={(event) => {
              void handleFileChange(event);
            }}
          />
        </label>
        {selectedName === null ? null : (
          <p className="text-muted-foreground text-sm">
            Selected: <span className="font-mono text-foreground">{selectedName}</span>
          </p>
        )}
        <div className="flex items-center gap-3">
          <Button disabled={!canUpload} onClick={handleUpload} type="button">
            {deploy.isPending ? 'Deploying…' : 'Deploy package'}
          </Button>
          {readError === null ? null : <p className="text-destructive text-sm">{readError}</p>}
        </div>
        <DeployOutcome error={deploy.error} isError={deploy.isError} result={deploy.data ?? null} />
      </div>

      <section className="space-y-3" aria-label="Loaded versions">
        <h3 className="font-medium text-foreground text-sm">Loaded versions</h3>
        <VersionsBody
          error={versions.error}
          isError={versions.isError}
          isLoading={versions.isLoading}
          onRetry={() => {
            void versions.refetch();
          }}
          versions={versions.data ?? []}
        />
      </section>
    </div>
  );
}

export type DeployOutcomeProps = {
  isError: boolean;
  error: unknown;
  result: LoadPackageResult | null;
};

export function DeployOutcome({ isError, error, result }: DeployOutcomeProps) {
  if (isError) {
    return <ErrorState error={error} message={deployErrorMessage(error)} title="Deploy failed" />;
  }

  if (result === null) {
    return null;
  }

  return (
    <div className="rounded-lg border border-primary/40 bg-primary-glow p-4">
      <h3 className="font-medium text-foreground text-sm">
        {result.freshlyLoaded ? 'Package deployed' : 'Already resident (idempotent)'}
      </h3>
      <dl className="mt-2 space-y-1 text-sm">
        <Detail label="Workflow type" value={result.workflowType} />
        <Detail label="Content hash" value={result.contentHash} />
        <Detail label="Route changed" value={result.routeChanged ? 'yes' : 'no'} />
      </dl>
    </div>
  );
}

export type VersionsBodyProps = {
  isError: boolean;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
  versions: WorkflowVersion[];
};

export function VersionsBody({ isError, isLoading, error, onRetry, versions }: VersionsBodyProps) {
  if (isError) {
    if (error instanceof ApiError && error.status === 404) {
      return (
        <EmptyState
          description="This cluster runs with [deploy] disabled. Start it with [deploy] enabled=true to upload and list packages; packages are otherwise pre-loaded at startup."
          title="Deploy is disabled"
        />
      );
    }

    return <ErrorState error={error} onRetry={onRetry} title="Could not load versions" />;
  }

  if (isLoading) {
    return <LoadingSkeleton />;
  }

  if (versions.length === 0) {
    return <EmptyState description="No package versions are loaded." title="No versions" />;
  }

  return (
    <ul className="space-y-2">
      {versions.map((version) => (
        <li
          key={`${version.workflowType}:${version.contentHash}`}
          className="flex items-center justify-between gap-4 rounded-md border border-border px-4 py-3"
        >
          <span className="flex flex-col">
            <span className="font-medium text-foreground text-sm">{version.workflowType}</span>
            <span className="font-mono text-muted-foreground text-xs">{version.contentHash}</span>
          </span>
          <span className="flex flex-col items-end text-xs">
            <span className="text-muted-foreground">{version.manifestVersion}</span>
            <span className={version.routeActive ? 'text-primary' : 'text-muted-foreground'}>
              {version.routeActive ? 'route-active' : 'inactive'}
            </span>
          </span>
        </li>
      ))}
    </ul>
  );
}

function deployErrorMessage(error: unknown): string | undefined {
  if (!(error instanceof ApiError)) {
    return undefined;
  }

  if (error.status === 404) {
    return 'Deploy is disabled on this cluster ([deploy] enabled=false). Packages are pre-loaded at startup.';
  }

  if (error.status === 403) {
    return 'Deploy denied: this console lacks the deployment-wide deploy grant.';
  }

  return undefined;
}

function Detail({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-2">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="font-mono text-foreground">{value}</dd>
    </div>
  );
}
