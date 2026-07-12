export type DeploymentRecord = {
  deploymentId: string;
  documentPath: string;
  contentHash: string;
  packageId: string;
  workflowType: string;
  taskQueue: string;
  workflowId: string | null;
  runId: string | null;
};
export type GuidedStepResult = { step: 'check' | 'emit' | 'package' | 'deploy'; detail: string };
export type GuidedDeployResult = { deployment: DeploymentRecord; steps: GuidedStepResult[] };
export type WorkerAvailability = {
  available: boolean;
  taskQueue: string;
  connectedWorkers: number;
  scaffoldHint: string | null;
};
export type RunStatus = {
  deployment: DeploymentRecord;
  deployedSource: string;
  drifted: boolean;
};

type Request = (path: string, init?: RequestInit, documentEndpoint?: boolean) => Promise<unknown>;

export function createGuidedFacade(request: Request) {
  return {
    async deploy(path: string, contentHash: string): Promise<GuidedDeployResult> {
      const value = expectRecord(
        await request('/awl/deploy', jsonInit('POST', { path, content_hash: contentHash }))
      );
      return {
        deployment: parseDeployment(value.deployment),
        steps: expectArray(value.steps).map((item) => {
          const step = expectRecord(item);
          const name = expectString(step.step) as GuidedStepResult['step'];
          if (!['check', 'emit', 'package', 'deploy'].includes(name)) {
            throw new Error('Invalid guided step');
          }
          return { step: name, detail: expectString(step.detail) };
        }),
      };
    },
    async workerAvailability(namespace: string, taskQueue: string): Promise<WorkerAvailability> {
      const value = expectRecord(
        await request(
          '/awl/workers/availability',
          jsonInit('POST', { namespace, task_queue: taskQueue })
        )
      );
      return {
        available: expectBoolean(value.available),
        taskQueue: expectString(value.task_queue),
        connectedWorkers: expectNumber(value.connected_workers),
        scaffoldHint: value.scaffold_hint === null ? null : expectString(value.scaffold_hint),
      };
    },
    async bindRun(
      deploymentId: string,
      workflowId: string,
      runId: string
    ): Promise<DeploymentRecord> {
      return parseDeployment(
        await request(
          `/awl/runs/${encodeURIComponent(deploymentId)}/binding`,
          jsonInit('POST', { workflow_id: workflowId, run_id: runId })
        )
      );
    },
    async runStatus(deploymentId: string): Promise<RunStatus> {
      const value = expectRecord(await request(`/awl/runs/${encodeURIComponent(deploymentId)}`));
      return {
        deployment: parseDeployment(value.deployment),
        deployedSource: expectString(value.deployed_source),
        drifted: expectBoolean(value.drifted),
      };
    },
  };
}

function parseDeployment(value: unknown): DeploymentRecord {
  const record = expectRecord(value);
  return {
    deploymentId: expectString(record.deployment_id),
    documentPath: expectString(record.document_path),
    contentHash: expectString(record.content_hash),
    packageId: expectString(record.package_id),
    workflowType: expectString(record.workflow_type),
    taskQueue: expectString(record.task_queue),
    workflowId: record.workflow_id === null ? null : expectString(record.workflow_id),
    runId: record.run_id === null ? null : expectString(record.run_id),
  };
}

function jsonInit(method: string, body: unknown): RequestInit {
  return { method, headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) };
}

function expectRecord(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new Error('Invalid guided authoring response: expected object');
  }
  return value as Record<string, unknown>;
}
function expectArray(value: unknown): unknown[] {
  if (!Array.isArray(value)) throw new Error('Invalid guided authoring response: expected array');
  return value;
}
function expectString(value: unknown): string {
  if (typeof value !== 'string')
    throw new Error('Invalid guided authoring response: expected string');
  return value;
}
function expectBoolean(value: unknown): boolean {
  if (typeof value !== 'boolean')
    throw new Error('Invalid guided authoring response: expected boolean');
  return value;
}
function expectNumber(value: unknown): number {
  if (typeof value !== 'number')
    throw new Error('Invalid guided authoring response: expected number');
  return value;
}
