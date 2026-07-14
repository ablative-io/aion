import { describe, expect, test } from 'bun:test';

import { createAuthoringFacade, GuidedFlowRefusedError } from './facade';

const deployment = {
  deployment_id: 'deploy-1',
  document_path: 'demo.awl',
  content_hash: 'a'.repeat(64),
  package_id: 'package-1',
  workflow_type: 'demo',
  task_queue: 'jobs',
  workflow_id: null,
  run_id: null,
};

describe('guided run boundary', () => {
  test('wires save, deploy, worker gate, run binding, and deployed-source status', async () => {
    const requests: string[] = [];
    const facade = createAuthoringFacade(async (input, init) => {
      const path = String(input);
      requests.push(`${init?.method ?? 'GET'} ${path}`);
      if (path.includes('/documents/')) {
        return Response.json({ source: 'deployed', content_hash: 'a'.repeat(64) });
      }
      if (path === '/awl/deploy') {
        return Response.json({
          deployment,
          steps: [
            { step: 'check', detail: 'green' },
            { step: 'compile', detail: 'direct BEAM compiled' },
            { step: 'package', detail: 'built' },
            { step: 'deploy', detail: 'loaded' },
          ],
        });
      }
      if (path.includes('/workers/availability')) {
        return Response.json({
          available: true,
          task_queue: 'jobs',
          connected_workers: 1,
          scaffold_hint: null,
        });
      }
      if (path.endsWith('/binding')) {
        return Response.json({ ...deployment, workflow_id: 'workflow-1', run_id: 'run-1' });
      }
      return Response.json({
        deployment: { ...deployment, workflow_id: 'workflow-1', run_id: 'run-1' },
        deployed_source: 'deployed',
        drifted: false,
      });
    });

    const hash = await facade.saveDocument('demo.awl', 'deployed');
    const shipped = await facade.deploy('demo.awl', hash);
    const worker = await facade.workerAvailability('default', shipped.deployment.taskQueue);
    const bound = await facade.bindRun(shipped.deployment.deploymentId, 'workflow-1', 'run-1');
    const status = await facade.runStatus(bound.deploymentId);

    expect(worker.available).toBe(true);
    expect(status.deployedSource).toBe('deployed');
    expect(status.deployment.contentHash).toBe('a'.repeat(64));
    expect(shipped.steps.map((step) => step.step)).toEqual([
      'check',
      'compile',
      'package',
      'deploy',
    ]);
    expect(requests).toEqual([
      'PUT /awl/documents/demo.awl',
      'POST /awl/deploy',
      'POST /awl/workers/availability',
      'POST /awl/runs/deploy-1/binding',
      'GET /awl/runs/deploy-1',
    ]);
  });

  test('preserves a typed server refusal instead of advancing', async () => {
    const facade = createAuthoringFacade(async () =>
      Response.json(
        { error_type: 'RevisionMismatch', message: 'saved revision changed' },
        { status: 422 }
      )
    );
    await expect(facade.deploy('demo.awl', 'old')).rejects.toBeInstanceOf(GuidedFlowRefusedError);
    try {
      await facade.deploy('demo.awl', 'old');
    } catch (error) {
      expect((error as GuidedFlowRefusedError).code).toBe('RevisionMismatch');
    }
  });
});
