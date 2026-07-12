import { Check, Circle, Diff, LoaderCircle, Play, Rocket, XCircle } from 'lucide-react';
import { useMemo, useState } from 'react';

import { Button } from '@/components/ui/button';
import { useNamespace } from '@/features/namespace';
import { useLiveWorkflowEvents, useWorkflowHistory } from '@/features/workflow-detail';
import { createConfiguredApiClient } from '@/lib/config';
import { cn } from '@/lib/utils';

import type { EditResult } from '../lib/facade';
import { authoringFacade, type GuidedStepResult, type RunStatus } from '../lib/facade';
import { activeStepForActivity, hasRevisionDrift } from '../lib/run-mode';
import { AuthoringCanvas } from './AuthoringCanvas';

const STEP_NAMES = ['check', 'emit', 'package', 'deploy', 'start', 'watch'] as const;
type StepName = (typeof STEP_NAMES)[number];
type StepState = { state: 'idle' | 'working' | 'done' | 'failed'; detail?: string };
type StepMap = Record<StepName, StepState>;

export type ShipRunPanelProps = {
  path: string;
  buffer: string;
  documents: readonly { path: string; name: string }[];
};

export function ShipRunPanel({ path, buffer, documents }: ShipRunPanelProps) {
  const { selectedNamespace } = useNamespace();
  const apiClient = useMemo(
    () => createConfiguredApiClient({ namespace: selectedNamespace }),
    [selectedNamespace]
  );
  const [steps, setSteps] = useState<StepMap>(() => initialSteps());
  const [run, setRun] = useState<RunStatus | null>(null);
  const [semantic, setSemantic] =
    useState<Awaited<ReturnType<typeof authoringFacade.check>>['semantic']>(null);
  const [input, setInput] = useState('{}');
  const [showDiff, setShowDiff] = useState(false);
  const [busy, setBusy] = useState(false);
  const runningWorkflowId = run?.deployment.workflowId ?? '';
  const history = useWorkflowHistory({
    enabled: run?.deployment.workflowId !== null && run !== null,
    workflowId: runningWorkflowId,
  });
  const live = useLiveWorkflowEvents({
    enabled: run?.deployment.workflowId !== null && run !== null,
    history: history.data ?? [],
    onResync: () => void history.refetch(),
    workflowId: runningWorkflowId,
  });
  const activeActivity = [...live.timeline]
    .reverse()
    .find((entry) => entry.kind === 'activity')?.activityType;
  const activeStep =
    semantic === null ? null : activeStepForActivity(semantic.graph, activeActivity);

  async function shipAndRun() {
    if (busy || selectedNamespace === null) return;
    setBusy(true);
    setRun(null);
    setSemantic(null);
    setSteps(initialSteps('check'));
    try {
      const checked = await authoringFacade.check(buffer, path);
      if (!checked.deploysGreen) {
        throw new Error(checked.diagnostics[0]?.message ?? 'Document does not deploy green');
      }
      complete('check', `${checked.steps ?? 0} steps deploy green`);
      const contentHash = await authoringFacade.saveDocument(path, buffer);
      working('emit');
      const deployed = await authoringFacade.deploy(path, contentHash);
      applyServerSteps(deployed.steps);
      working('start');
      const availability = await authoringFacade.workerAvailability(
        selectedNamespace,
        deployed.deployment.taskQueue
      );
      if (!availability.available) {
        throw new Error(
          availability.scaffoldHint ?? `No worker is connected to ${availability.taskQueue}`
        );
      }
      const parsedInput: unknown = JSON.parse(input);
      if (typeof parsedInput !== 'object' || parsedInput === null || Array.isArray(parsedInput)) {
        throw new Error('Run input must be a JSON object');
      }
      const runInput = parsedInput as Record<string, unknown>;
      const started = await apiClient.startWorkflow(
        {
          workflowType: deployed.deployment.workflowType,
          input: runInput,
          taskQueue: deployed.deployment.taskQueue,
        },
        { namespace: selectedNamespace }
      );
      complete('start', `run ${started.runId} started`);
      working('watch');
      await authoringFacade.bindRun(
        deployed.deployment.deploymentId,
        started.workflowId,
        started.runId
      );
      const status = await authoringFacade.runStatus(deployed.deployment.deploymentId);
      const deployedCheck = await authoringFacade.check(status.deployedSource, path);
      if (deployedCheck.semantic === null)
        throw new Error('Deployed revision could not be projected');
      setSemantic(deployedCheck.semantic);
      setRun(status);
      complete('watch', 'live event stream attached');
    } catch (error) {
      failCurrent(error instanceof Error ? error.message : 'Guided run failed');
    } finally {
      setBusy(false);
    }
  }

  function update(name: StepName, next: StepState) {
    setSteps((current) => ({ ...current, [name]: next }));
  }
  function working(name: StepName) {
    update(name, { state: 'working' });
  }
  function complete(name: StepName, detail: string) {
    update(name, { state: 'done', detail });
  }
  function applyServerSteps(results: GuidedStepResult[]) {
    for (const result of results) complete(result.step, result.detail);
  }
  function failCurrent(detail: string) {
    setSteps((current) => {
      const name = STEP_NAMES.find((step) => current[step].state === 'working') ?? 'check';
      return { ...current, [name]: { state: 'failed', detail } };
    });
  }

  return (
    <section
      className="flex min-h-[32rem] min-w-0 flex-1 flex-col bg-surface-elevated"
      aria-label="Ship and run"
    >
      <div className="grid gap-3 border-border border-b bg-surface-base p-4 lg:grid-cols-[1fr_18rem]">
        <ol className="grid grid-cols-2 gap-2 md:grid-cols-3 xl:grid-cols-6">
          {STEP_NAMES.map((name) => (
            <Step key={name} name={name} value={steps[name]} />
          ))}
        </ol>
        <div className="flex flex-col gap-2">
          <label className="font-medium text-foreground text-xs" htmlFor="run-input">
            Run input
          </label>
          <textarea
            className="min-h-16 rounded-md border border-border bg-surface-elevated p-2 font-mono text-foreground text-xs outline-none focus-visible:ring-2 focus-visible:ring-accent-primary"
            id="run-input"
            onChange={(event) => setInput(event.target.value)}
            value={input}
          />
          <Button disabled={busy || selectedNamespace === null} onClick={() => void shipAndRun()}>
            {busy ? (
              <LoaderCircle className="size-4 animate-spin" />
            ) : (
              <Rocket className="size-4" />
            )}
            {busy ? 'Shipping…' : 'Ship & Run'}
          </Button>
        </div>
      </div>
      {run === null || semantic === null ? (
        <div className="m-auto flex max-w-md flex-col items-center gap-3 p-8 text-center">
          <Play className="size-7 text-accent-primary" />
          <h3 className="font-semibold text-foreground">Run mode is bound at deploy</h3>
          <p className="text-muted-foreground text-sm">
            Check, emit, package, deploy, start, and attach the live stream without leaving this
            surface.
          </p>
        </div>
      ) : (
        <>
          <RevisionDriftNotice
            deployed={run.deployedSource}
            editor={buffer}
            onToggle={() => setShowDiff((shown) => !shown)}
            showDiff={showDiff}
          />
          <AuthoringCanvas
            diagnostics={[]}
            documents={documents}
            graph={semantic.graph}
            mode="run"
            onGesture={refuseGesture}
            onJumpToSpan={() => undefined}
            onOpenDocument={() => undefined}
            path={path}
            routeTargets={semantic.graph.steps.map((step) => step.name)}
            selectedStep={activeStep}
          />
        </>
      )}
    </section>
  );
}

function Step({ name, value }: { name: StepName; value: StepState }) {
  const Icon =
    value.state === 'done'
      ? Check
      : value.state === 'failed'
        ? XCircle
        : value.state === 'working'
          ? LoaderCircle
          : Circle;
  return (
    <li
      className={cn(
        'rounded-lg border p-2',
        value.state === 'failed'
          ? 'border-destructive/40 bg-destructive/10'
          : value.state === 'done'
            ? 'border-success/40 bg-success/10'
            : 'border-border bg-surface-elevated'
      )}
    >
      <div className="flex items-center gap-2 font-medium text-foreground text-xs capitalize">
        <Icon className={cn('size-3.5', value.state === 'working' && 'animate-spin')} />
        {name}
      </div>
      {value.detail !== undefined && (
        <p className="mt-1 break-words text-muted-foreground text-[0.6875rem]">{value.detail}</p>
      )}
    </li>
  );
}

export function RevisionDriftNotice({
  deployed,
  editor,
  onToggle,
  showDiff,
}: {
  deployed: string;
  editor: string;
  onToggle: () => void;
  showDiff: boolean;
}) {
  if (!hasRevisionDrift(editor, deployed)) return null;
  return (
    <>
      <div
        className="flex items-center justify-between gap-3 border-warning/40 border-b bg-warning/10 px-4 py-2 text-warning text-sm"
        role="status"
      >
        <strong>editor has drifted from the running revision</strong>
        <Button onClick={onToggle} size="sm" variant="outline">
          <Diff className="size-4" />
          {showDiff ? 'Hide diff' : 'View diff'}
        </Button>
      </div>
      {showDiff && <RevisionDiff deployed={deployed} editor={editor} />}
    </>
  );
}

function RevisionDiff({ deployed, editor }: { deployed: string; editor: string }) {
  return (
    <div className="grid max-h-64 grid-cols-2 overflow-auto border-warning/30 border-b bg-surface-base font-mono text-xs">
      <pre className="border-border border-r p-3 text-muted-foreground">
        <strong className="text-foreground">Deployed revision</strong>
        {'\n'}
        {deployed}
      </pre>
      <pre className="p-3 text-muted-foreground">
        <strong className="text-foreground">Editor buffer</strong>
        {'\n'}
        {editor}
      </pre>
    </div>
  );
}

function initialSteps(working?: StepName): StepMap {
  return Object.fromEntries(
    STEP_NAMES.map((name) => [name, { state: name === working ? 'working' : 'idle' }])
  ) as StepMap;
}

async function refuseGesture(): Promise<EditResult> {
  throw new Error('Run mode is read-only');
}
