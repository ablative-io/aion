import { GitBranchPlus, ListPlus, Pencil, Plus, Replace, Trash2, X } from 'lucide-react';
import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import { ACTION_IDS } from '@/lib/keybindings/actions';
import { useAction } from '@/lib/keybindings/context';

import type { ActionParameter, GestureOperation, RouteArgument } from '../lib/facade';
import type { GraphProjection } from '../lib/projection-types';

type GestureDialog = 'step' | 'action' | 'route' | 'fall_through' | 'rename' | 'delete';

export type CanvasGestureControlsProps = {
  graph: GraphProjection;
  selectedStep: string | null;
  disabled: boolean;
  routeTargets: readonly string[];
  onGesture: (operation: GestureOperation) => Promise<unknown>;
  onBeginProseEdit: (step: string) => void;
};

export function CanvasGestureControls({
  graph,
  selectedStep,
  disabled,
  routeTargets,
  onGesture,
  onBeginProseEdit,
}: CanvasGestureControlsProps) {
  const [dialog, setDialog] = useState<GestureDialog | null>(null);
  const openForSelection = (next: GestureDialog) => {
    if (selectedStep !== null) setDialog(next);
  };
  const editProse = () => {
    if (selectedStep !== null) onBeginProseEdit(selectedStep);
  };

  useAction(ACTION_IDS.authoringAddStep, () => setDialog('step'));
  useAction(ACTION_IDS.authoringAddAction, () => setDialog('action'));
  useAction(ACTION_IDS.authoringAddRoute, () => openForSelection('route'));
  useAction(ACTION_IDS.authoringAddFallThrough, () => openForSelection('fall_through'));
  useAction(ACTION_IDS.authoringEditProse, editProse);
  useAction(ACTION_IDS.authoringRenameBinding, () => openForSelection('rename'));
  useAction(ACTION_IDS.authoringDeleteStep, () => openForSelection('delete'));

  return (
    <>
      <div className="absolute top-3 left-3 z-10 flex flex-wrap gap-1.5 rounded-lg border border-border bg-surface-base/95 p-1.5 shadow-sm backdrop-blur">
        <ToolbarButton
          disabled={disabled}
          icon={<Plus />}
          label="Add step"
          onClick={() => setDialog('step')}
        />
        <ToolbarButton
          disabled={disabled}
          icon={<ListPlus />}
          label="Add action"
          onClick={() => setDialog('action')}
        />
        <ToolbarButton
          disabled={disabled || selectedStep === null}
          icon={<GitBranchPlus />}
          label="Route"
          onClick={() => openForSelection('route')}
        />
        <ToolbarButton
          disabled={disabled || selectedStep === null}
          icon={<GitBranchPlus />}
          label="Fall-through"
          onClick={() => openForSelection('fall_through')}
        />
        <ToolbarButton
          disabled={disabled || selectedStep === null}
          icon={<Pencil />}
          label="Edit prose"
          onClick={editProse}
        />
        <ToolbarButton
          disabled={disabled || selectedStep === null}
          icon={<Replace />}
          label="Rename"
          onClick={() => openForSelection('rename')}
        />
        <ToolbarButton
          destructive
          disabled={disabled || selectedStep === null}
          icon={<Trash2 />}
          label="Delete"
          onClick={() => openForSelection('delete')}
        />
      </div>
      {dialog !== null && (
        <GestureForm
          dialog={dialog}
          graph={graph}
          routeTargets={routeTargets}
          selectedStep={selectedStep}
          onClose={() => setDialog(null)}
          onGesture={onGesture}
        />
      )}
    </>
  );
}

function ToolbarButton({
  destructive = false,
  disabled,
  icon,
  label,
  onClick,
}: {
  destructive?: boolean;
  disabled: boolean;
  icon: React.ReactElement<{ className?: string }>;
  label: string;
  onClick: () => void;
}) {
  return (
    <Button
      aria-label={label}
      className={destructive ? 'text-destructive hover:text-destructive' : undefined}
      disabled={disabled}
      onClick={onClick}
      size="sm"
      type="button"
      variant="ghost"
    >
      <span className="[&>svg]:size-3.5">{icon}</span>
      <span className="hidden xl:inline">{label}</span>
    </Button>
  );
}

function GestureForm({
  dialog,
  graph,
  routeTargets,
  selectedStep,
  onClose,
  onGesture,
}: {
  dialog: GestureDialog;
  graph: GraphProjection;
  routeTargets: readonly string[];
  selectedStep: string | null;
  onClose: () => void;
  onGesture: (operation: GestureOperation) => Promise<unknown>;
}) {
  const [name, setName] = useState('');
  const [prose, setProse] = useState('');
  const [worker, setWorker] = useState('');
  const [returnType, setReturnType] = useState('String');
  const [params, setParams] = useState<ActionParameter[]>([]);
  const [target, setTarget] = useState(
    routeTargets.find((candidate) => candidate !== selectedStep) ?? ''
  );
  const [payload, setPayload] = useState<RouteArgument[]>([]);
  const [guardType, setGuardType] = useState<'when' | 'otherwise'>('otherwise');
  const [expression, setExpression] = useState('true');
  const [renameKind, setRenameKind] = useState<'step' | 'binding'>('step');
  const [from, setFrom] = useState(selectedStep ?? '');
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (dialog === 'route') setName(target === '' ? 'next' : `to_${target}`);
  }, [dialog, target]);

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    const operation = operationFor({
      dialog,
      selectedStep,
      name,
      prose,
      worker,
      returnType,
      params,
      target,
      payload,
      guardType,
      expression,
      renameKind,
      from,
    });
    if (operation === null) return;
    setSubmitting(true);
    try {
      await onGesture(operation);
      onClose();
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="absolute inset-0 z-20 flex items-start justify-center bg-surface-base/60 p-16 backdrop-blur-sm">
      <form
        aria-label={`${dialogLabel(dialog)} gesture`}
        className="w-full max-w-md rounded-xl border border-border bg-surface-elevated p-4 shadow-xl"
        onSubmit={(event) => void submit(event)}
      >
        <header className="mb-4 flex items-center justify-between gap-3">
          <h3 className="font-semibold text-foreground text-sm">{dialogLabel(dialog)}</h3>
          <Button
            aria-label="Close gesture editor"
            onClick={onClose}
            size="icon"
            type="button"
            variant="ghost"
          >
            <X className="size-4" />
          </Button>
        </header>
        {dialog === 'step' && (
          <>
            <Field label="Step name" onChange={setName} required value={name} />
            <Field label="Node prose" onChange={setProse} value={prose} />
          </>
        )}
        {dialog === 'action' && (
          <>
            <Field label="Worker" onChange={setWorker} required value={worker} />
            <Field label="Action name" onChange={setName} required value={name} />
            <ParameterEditor params={params} setParams={setParams} />
            <Field label="Return type" onChange={setReturnType} required value={returnType} />
          </>
        )}
        {(dialog === 'route' || dialog === 'fall_through') && (
          <TargetSelect
            routeTargets={dialog === 'route' ? routeTargets : graph.steps.map((step) => step.name)}
            selectedStep={selectedStep}
            setTarget={setTarget}
            target={target}
          />
        )}
        {dialog === 'route' && (
          <>
            <Field label="Outcome name" onChange={setName} required value={name} />
            <label className="mb-3 block text-muted-foreground text-xs">
              Guard
              <select
                className={inputClass}
                onChange={(event) => setGuardType(event.target.value as 'when' | 'otherwise')}
                value={guardType}
              >
                <option value="otherwise">otherwise</option>
                <option value="when">when</option>
              </select>
            </label>
            {guardType === 'when' && (
              <Field label="When expression" onChange={setExpression} required value={expression} />
            )}
            <RoutePayloadEditor payload={payload} setPayload={setPayload} />
          </>
        )}
        {dialog === 'rename' && (
          <>
            <label className="mb-3 block text-muted-foreground text-xs">
              Declaration kind
              <select
                className={inputClass}
                onChange={(event) => setRenameKind(event.target.value as 'step' | 'binding')}
                value={renameKind}
              >
                <option value="step">step</option>
                <option value="binding">value binding</option>
              </select>
            </label>
            <Field label="Current name" onChange={setFrom} required value={from} />
            <Field label="New name" onChange={setName} required value={name} />
          </>
        )}
        {dialog === 'delete' && (
          <p className="mb-4 text-foreground text-sm">
            Delete step <strong className="font-mono">{selectedStep}</strong>? Referenced steps are
            refused by the server.
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button onClick={onClose} type="button" variant="outline">
            Cancel
          </Button>
          <Button
            disabled={submitting}
            type="submit"
            variant={dialog === 'delete' ? 'destructive' : 'default'}
          >
            {submitting ? 'Applying…' : dialog === 'delete' ? 'Delete step' : 'Apply gesture'}
          </Button>
        </div>
      </form>
    </div>
  );
}

const inputClass =
  'mt-1 block w-full rounded-md border border-border bg-surface-base px-2.5 py-2 font-mono text-foreground text-sm outline-none focus-visible:ring-2 focus-visible:ring-accent-primary';

function Field({
  label,
  onChange,
  required = false,
  value,
}: {
  label: string;
  onChange: (value: string) => void;
  required?: boolean;
  value: string;
}) {
  return (
    <label className="mb-3 block text-muted-foreground text-xs">
      {label}
      <input
        className={inputClass}
        onChange={(event) => onChange(event.target.value)}
        required={required}
        value={value}
      />
    </label>
  );
}

function ParameterEditor({
  params,
  setParams,
}: {
  params: ActionParameter[];
  setParams: (params: ActionParameter[]) => void;
}) {
  return (
    <fieldset className="mb-3 rounded-lg border border-border p-2">
      <legend className="px-1 text-muted-foreground text-xs">Parameter types</legend>
      {params.map((parameter, index) => (
        <div className="mb-2 grid grid-cols-2 gap-2" key={`${parameter.name}:${parameter.type}`}>
          <input
            aria-label={`Parameter ${index + 1} name`}
            className={inputClass}
            onChange={(event) =>
              setParams(
                params.map((item, itemIndex) =>
                  itemIndex === index ? { ...item, name: event.target.value } : item
                )
              )
            }
            placeholder="name"
            value={parameter.name}
          />
          <input
            aria-label={`Parameter ${index + 1} type`}
            className={inputClass}
            onChange={(event) =>
              setParams(
                params.map((item, itemIndex) =>
                  itemIndex === index ? { ...item, type: event.target.value } : item
                )
              )
            }
            placeholder="Type"
            value={parameter.type}
          />
        </div>
      ))}
      <Button
        onClick={() => setParams([...params, { name: '', type: 'String' }])}
        size="sm"
        type="button"
        variant="outline"
      >
        Add parameter
      </Button>
    </fieldset>
  );
}

function RoutePayloadEditor({
  payload,
  setPayload,
}: {
  payload: RouteArgument[];
  setPayload: (payload: RouteArgument[]) => void;
}) {
  return (
    <fieldset className="mb-3 rounded-lg border border-border p-2">
      <legend className="px-1 text-muted-foreground text-xs">Route payload</legend>
      {payload.map((argument, index) => (
        <div
          className="mb-2 grid grid-cols-2 gap-2"
          key={`${argument.name}:${argument.expression}`}
        >
          <input
            aria-label={`Route payload ${index + 1} name`}
            className={inputClass}
            onChange={(event) =>
              setPayload(
                payload.map((item, itemIndex) =>
                  itemIndex === index ? { ...item, name: event.target.value } : item
                )
              )
            }
            placeholder="field"
            value={argument.name}
          />
          <input
            aria-label={`Route payload ${index + 1} expression`}
            className={inputClass}
            onChange={(event) =>
              setPayload(
                payload.map((item, itemIndex) =>
                  itemIndex === index ? { ...item, expression: event.target.value } : item
                )
              )
            }
            placeholder="expression"
            value={argument.expression}
          />
        </div>
      ))}
      <Button
        onClick={() => setPayload([...payload, { name: '', expression: '' }])}
        size="sm"
        type="button"
        variant="outline"
      >
        Add payload field
      </Button>
    </fieldset>
  );
}

function TargetSelect({
  routeTargets,
  selectedStep,
  target,
  setTarget,
}: {
  routeTargets: readonly string[];
  selectedStep: string | null;
  target: string;
  setTarget: (target: string) => void;
}) {
  return (
    <label className="mb-3 block text-muted-foreground text-xs">
      Target step
      <select
        className={inputClass}
        onChange={(event) => setTarget(event.target.value)}
        required
        value={target}
      >
        {routeTargets
          .filter((name) => name !== selectedStep)
          .map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
      </select>
    </label>
  );
}

function dialogLabel(dialog: GestureDialog) {
  return {
    step: 'Add step',
    action: 'Add action contract',
    route: 'Draw outcome route',
    fall_through: 'Draw fall-through',
    rename: 'Rename binding',
    delete: 'Confirm deletion',
  }[dialog];
}

function operationFor(input: {
  dialog: GestureDialog;
  selectedStep: string | null;
  name: string;
  prose: string;
  worker: string;
  returnType: string;
  params: ActionParameter[];
  target: string;
  payload: RouteArgument[];
  guardType: 'when' | 'otherwise';
  expression: string;
  renameKind: 'step' | 'binding';
  from: string;
}): GestureOperation | null {
  if (input.dialog === 'step') return { type: 'add_step', name: input.name, prose: input.prose };
  if (input.dialog === 'action')
    return {
      type: 'add_action',
      worker: input.worker,
      name: input.name,
      params: input.params,
      return_type: input.returnType,
    };
  if (input.dialog === 'rename')
    return { type: 'rename_binding', kind: input.renameKind, from: input.from, to: input.name };
  if (input.selectedStep === null) return null;
  if (input.dialog === 'fall_through')
    return { type: 'add_fall_through', source: input.selectedStep, target: input.target };
  if (input.dialog === 'delete') return { type: 'delete_step', step: input.selectedStep };
  return {
    type: 'add_outcome_route',
    source: input.selectedStep,
    target: input.target,
    name: input.name,
    guard:
      input.guardType === 'when'
        ? { type: 'when', expression: input.expression }
        : { type: 'otherwise' },
    payload: input.payload,
  };
}
