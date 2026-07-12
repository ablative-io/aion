import { Plus, Trash2 } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';

import type { AwlDocument, EditResult, GestureOperation } from '../lib/facade';
import type { SemanticIndex, StudioProjection } from '../lib/projection-types';
import { ProjectionPane } from './ProjectionPane';

const control =
  'rounded-md border border-border bg-surface-base px-2 py-1.5 text-foreground text-xs outline-none focus:ring-2 focus:ring-accent-primary';

type Props = {
  semantic: SemanticIndex;
  documents: readonly AwlDocument[];
  path: string;
  selectedStep: string | null;
  onGesture: (operation: GestureOperation) => Promise<EditResult>;
  onJumpToSpan: (offset: number) => void;
  onOpenDocument: (path: string) => void;
};

export function StudioPane(props: Props) {
  const { studio } = props.semantic;
  const [error, setError] = useState<string | null>(null);
  const apply = async (operation: GestureOperation) => {
    setError(null);
    try {
      await props.onGesture(operation);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Structured edit refused');
    }
  };
  return (
    <section
      className="flex min-w-0 flex-1 flex-col overflow-auto bg-surface-elevated p-4"
      aria-label="Studio"
    >
      {error !== null && (
        <p
          className="mb-3 rounded-md border border-destructive/30 bg-destructive/10 p-2 text-destructive text-xs"
          role="alert"
        >
          {error}
        </p>
      )}
      <div className="grid gap-4 xl:grid-cols-2">
        <StudioSection
          description="Declared records and enums. Schema declarations remain text-editable."
          title="Types"
        >
          <TypeForms studio={studio} apply={apply} />
          <TypeList studio={studio} apply={apply} />
        </StudioSection>
        <StudioSection
          description="Runtime queues and their typed action contracts."
          title="Workers & Actions"
        >
          <WorkerForms studio={studio} apply={apply} />
          <WorkerList studio={studio} apply={apply} />
        </StudioSection>
      </div>
      <section className="mt-4 min-h-[30rem] overflow-hidden rounded-xl border border-border bg-surface-base">
        <header className="border-border border-b px-4 py-3">
          <h3 className="font-semibold text-foreground">Steps</h3>
          <p className="text-muted-foreground text-xs">
            Compose steps and routes on the existing projectional canvas.
          </p>
        </header>
        <div className="flex h-[32rem]">
          <ProjectionPane
            check={null}
            documents={props.documents}
            onGesture={props.onGesture}
            onJumpToSpan={props.onJumpToSpan}
            onOpenDocument={props.onOpenDocument}
            path={props.path}
            selectedStep={props.selectedStep}
            semantic={props.semantic}
            viewMode="canvas"
          />
        </div>
      </section>
    </section>
  );
}

function StudioSection({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-xl border border-border bg-surface-base p-4">
      <h3 className="font-semibold text-foreground">{title}</h3>
      <p className="mb-3 text-muted-foreground text-xs">{description}</p>
      {children}
    </section>
  );
}

function TypeForms({
  studio,
  apply,
}: {
  studio: StudioProjection;
  apply: (operation: GestureOperation) => Promise<void>;
}) {
  const [name, setName] = useState('');
  const [field, setField] = useState('value');
  const [fieldType, setFieldType] = useState('String');
  const [variants, setVariants] = useState('Pending, Complete');
  return (
    <div className="grid gap-2 rounded-lg border border-border-subtle bg-surface-elevated p-3 sm:grid-cols-2">
      <input
        aria-label="Type name"
        className={control}
        onChange={(event) => setName(event.target.value)}
        placeholder="Order"
        value={name}
      />
      <div className="flex gap-2">
        <input
          aria-label="Initial field name"
          className={`${control} min-w-0 flex-1`}
          onChange={(event) => setField(event.target.value)}
          value={field}
        />
        <TypePicker studio={studio} value={fieldType} onChange={setFieldType} />
      </div>
      <Button
        disabled={!name || !field}
        onClick={() =>
          void apply({ type: 'add_type', name, fields: [{ name: field, type: fieldType }] })
        }
        size="sm"
      >
        <Plus className="size-3.5" />
        Record
      </Button>
      <div className="flex gap-2">
        <input
          aria-label="Enum variants"
          className={`${control} min-w-0 flex-1`}
          onChange={(event) => setVariants(event.target.value)}
          value={variants}
        />
        <Button
          disabled={!name || !variants}
          onClick={() =>
            void apply({
              type: 'add_enum_type',
              name,
              variants: variants
                .split(',')
                .map((item) => item.trim())
                .filter(Boolean),
            })
          }
          size="sm"
          variant="outline"
        >
          Enum
        </Button>
      </div>
    </div>
  );
}

function TypeList({
  studio,
  apply,
}: {
  studio: StudioProjection;
  apply: (operation: GestureOperation) => Promise<void>;
}) {
  return (
    <ul className="mt-3 space-y-2">
      {studio.types.map((type) => (
        <li className="rounded-lg border border-border p-3" key={type.name}>
          <div className="flex items-center justify-between">
            <strong className="font-mono text-foreground text-sm">{type.name}</strong>
            <span className="text-muted-foreground text-xs">{type.kind}</span>
          </div>
          {type.kind === 'record' && (
            <>
              <ul className="my-2 space-y-1">
                {type.fields.map((field) => (
                  <li className="flex items-center justify-between text-xs" key={field.name}>
                    <code>
                      {field.name}: {field.type}
                    </code>
                    <button
                      aria-label={`Remove ${field.name}`}
                      className="text-muted-foreground hover:text-destructive"
                      onClick={() =>
                        confirmAndRun(`Remove field ${field.name}?`, () =>
                          apply({
                            type: 'remove_type_field',
                            type_name: type.name,
                            name: field.name,
                          })
                        )
                      }
                      type="button"
                    >
                      <Trash2 className="size-3.5" />
                    </button>
                  </li>
                ))}
              </ul>
              <AddField typeName={type.name} studio={studio} apply={apply} />
            </>
          )}
          {type.kind === 'enum' && (
            <p className="mt-2 font-mono text-muted-foreground text-xs">
              {type.variants.join(' | ')}
            </p>
          )}
          {type.kind === 'schema' && (
            <p className="mt-2 text-muted-foreground text-xs">Edit schema details in Text view.</p>
          )}
        </li>
      ))}
    </ul>
  );
}

function AddField({
  typeName,
  studio,
  apply,
}: {
  typeName: string;
  studio: StudioProjection;
  apply: (operation: GestureOperation) => Promise<void>;
}) {
  const [name, setName] = useState('');
  const [type, setType] = useState('String');
  return (
    <div className="flex gap-2">
      <input
        aria-label={`New field for ${typeName}`}
        className={`${control} min-w-0 flex-1`}
        onChange={(event) => setName(event.target.value)}
        placeholder="field"
        value={name}
      />
      <TypePicker studio={studio} value={type} onChange={setType} />
      <Button
        disabled={!name}
        onClick={() =>
          void apply({ type: 'add_type_field', type_name: typeName, name, field_type: type })
        }
        size="sm"
        variant="outline"
      >
        <Plus className="size-3.5" />
      </Button>
    </div>
  );
}

function WorkerForms({
  studio,
  apply,
}: {
  studio: StudioProjection;
  apply: (operation: GestureOperation) => Promise<void>;
}) {
  const [worker, setWorker] = useState('');
  return (
    <div className="rounded-lg border border-border-subtle bg-surface-elevated p-3">
      <label className="mb-1 block text-muted-foreground text-xs" htmlFor="worker-name">
        Add worker with its first action
      </label>
      <input
        className={`${control} mb-2 w-full`}
        id="worker-name"
        onChange={(event) => setWorker(event.target.value)}
        placeholder="orders"
        value={worker}
      />
      <ActionForm
        studio={studio}
        label="Add worker"
        onSubmit={(action) => apply({ type: 'add_worker', name: worker, action })}
        disabled={!worker}
      />
    </div>
  );
}

function WorkerList({
  studio,
  apply,
}: {
  studio: StudioProjection;
  apply: (operation: GestureOperation) => Promise<void>;
}) {
  return (
    <ul className="mt-3 space-y-2">
      {studio.workers.map((worker) => (
        <li className="rounded-lg border border-border p-3" key={worker.name}>
          <div className="flex items-center justify-between">
            <strong className="font-mono text-foreground text-sm">{worker.name}</strong>
            <button
              className="text-muted-foreground hover:text-destructive"
              aria-label={`Remove worker ${worker.name}`}
              onClick={() =>
                confirmAndRun(`Remove worker ${worker.name} and all actions?`, () =>
                  apply({ type: 'remove_worker', name: worker.name })
                )
              }
              type="button"
            >
              <Trash2 className="size-3.5" />
            </button>
          </div>
          <ul className="my-2 space-y-1">
            {worker.actions.map((action) => (
              <li className="flex items-center justify-between gap-2 text-xs" key={action.name}>
                <code className="truncate">
                  {action.name}(
                  {action.params
                    .map((parameter) => `${parameter.name}: ${parameter.type}`)
                    .join(', ')}
                  ) → {action.returnType}
                </code>
                <button
                  aria-label={`Remove action ${action.name}`}
                  className="text-muted-foreground hover:text-destructive"
                  onClick={() =>
                    confirmAndRun(`Remove action ${action.name}?`, () =>
                      apply({ type: 'remove_action', worker: worker.name, name: action.name })
                    )
                  }
                  type="button"
                >
                  <Trash2 className="size-3.5" />
                </button>
              </li>
            ))}
          </ul>
          <ActionForm
            studio={studio}
            label="Add action"
            onSubmit={(action) =>
              apply({
                type: 'add_action',
                worker: worker.name,
                name: action.name,
                params: action.params,
                return_type: action.return_type,
              })
            }
          />
        </li>
      ))}
    </ul>
  );
}

function ActionForm({
  studio,
  label,
  onSubmit,
  disabled = false,
}: {
  studio: StudioProjection;
  label: string;
  onSubmit: (action: {
    name: string;
    params: { name: string; type: string }[];
    return_type: string;
  }) => Promise<void>;
  disabled?: boolean;
}) {
  const [name, setName] = useState('');
  const [parameter, setParameter] = useState('');
  const [parameterType, setParameterType] = useState('String');
  const [returnType, setReturnType] = useState('String');
  return (
    <div className="grid gap-2 sm:grid-cols-2">
      <input
        aria-label={`${label} name`}
        className={control}
        onChange={(event) => setName(event.target.value)}
        placeholder="process"
        value={name}
      />
      <div className="flex gap-2">
        <input
          aria-label="Parameter name"
          className={`${control} min-w-0 flex-1`}
          onChange={(event) => setParameter(event.target.value)}
          placeholder="input (optional)"
          value={parameter}
        />
        <TypePicker studio={studio} value={parameterType} onChange={setParameterType} />
      </div>
      <span className="flex items-center gap-2 text-muted-foreground text-xs">
        Returns <TypePicker studio={studio} value={returnType} onChange={setReturnType} />
      </span>
      <Button
        disabled={disabled || !name}
        onClick={() =>
          void onSubmit({
            name,
            params: parameter ? [{ name: parameter, type: parameterType }] : [],
            return_type: returnType,
          })
        }
        size="sm"
        variant="outline"
      >
        <Plus className="size-3.5" />
        {label}
      </Button>
    </div>
  );
}

function TypePicker({
  studio,
  value,
  onChange,
}: {
  studio: StudioProjection;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <select
      aria-label="Type"
      className={control}
      onChange={(event) => onChange(event.target.value)}
      value={value}
    >
      {[...studio.builtins, ...studio.types.map((type) => type.name)].map((name) => (
        <option key={name}>{name}</option>
      ))}
    </select>
  );
}

function confirmAndRun(message: string, action: () => Promise<void>) {
  if (window.confirm(message)) void action();
}
