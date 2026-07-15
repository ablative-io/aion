import { ExternalLink, Workflow } from 'lucide-react';
import { useEffect, useState } from 'react';
import { Handle, type NodeProps, Position } from 'reactflow';

import { cn } from '@/lib/utils';
import type { ProjectionStep } from '../lib/projection-types';
import { StepNodeBody } from './StepNodeBody';

export type CanvasNodeData = {
  kind: 'step' | 'child';
  name: string;
  label: string;
  step?: ProjectionStep;
  expandedSubflows?: ReadonlySet<string>;
  onToggleSubflow?: ((path: string) => void) | undefined;
  onJumpTo?: ((byteOffset: number) => void) | undefined;
  diagnostic?: 'primary' | 'cascade' | undefined;
  childDocumentPath?: string | undefined;
  onActivate: () => void;
  proseEditing?: boolean;
  onBeginProseEdit?: (() => void) | undefined;
  onCancelProseEdit?: (() => void) | undefined;
  onSaveProse?: ((prose: string) => Promise<void>) | undefined;
  onOpenChild?: (() => void) | undefined;
};

export function StepCanvasNode({ data, selected }: NodeProps<CanvasNodeData>) {
  const [draft, setDraft] = useState(data.label);
  const [saving, setSaving] = useState(false);
  useEffect(() => setDraft(data.label), [data.label]);

  const saveProse = async () => {
    if (data.onSaveProse === undefined) return;
    setSaving(true);
    try {
      await data.onSaveProse(draft);
    } finally {
      setSaving(false);
    }
  };
  return (
    <div>
      <Handle
        className="!size-2 !border-surface-elevated !bg-muted-foreground"
        position={Position.Top}
        type="target"
      />
      {data.proseEditing === true ? (
        <article
          aria-label={`Step ${data.name}`}
          className="min-w-52 max-w-64 rounded-xl border border-accent-primary bg-surface-elevated shadow-md"
        >
          <form
            aria-label={`Edit prose for ${data.name}`}
            className="nodrag p-3"
            onSubmit={(event) => {
              event.preventDefault();
              void saveProse();
            }}
          >
            <label className="block text-muted-foreground text-xs">
              Step prose
              <textarea
                className="mt-2 min-h-20 w-full resize-y rounded-md border border-border bg-surface-base p-2 text-foreground text-sm outline-none focus-visible:ring-2 focus-visible:ring-accent-primary"
                onChange={(event) => setDraft(event.target.value)}
                value={draft}
              />
            </label>
            <span className="mt-2 flex justify-end gap-2">
              <button
                className="rounded-md px-2 py-1 text-muted-foreground text-xs hover:bg-accent"
                onClick={data.onCancelProseEdit}
                type="button"
              >
                Cancel
              </button>
              <button
                className="rounded-md bg-primary px-2 py-1 text-primary-foreground text-xs disabled:opacity-50"
                disabled={saving}
                type="submit"
              >
                {saving ? 'Saving…' : 'Save prose'}
              </button>
            </span>
          </form>
        </article>
      ) : data.step !== undefined ? (
        <>
          <StepNodeBody
            diagnostic={data.diagnostic}
            expanded={data.expandedSubflows ?? new Set()}
            onJumpTo={data.onJumpTo ?? (() => data.onActivate())}
            onToggleSubflow={data.onToggleSubflow ?? (() => undefined)}
            path={data.step.name}
            selected={selected}
            step={data.step}
          />
          {data.onBeginProseEdit !== undefined && (
            <button
              aria-label={`Edit prose for ${data.name}`}
              className="nodrag mt-1 rounded-md px-2 py-1 text-muted-foreground text-xs outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-accent-primary"
              onClick={data.onBeginProseEdit}
              type="button"
            >
              Edit prose
            </button>
          )}
        </>
      ) : null}
      <Handle
        className="!size-2 !border-surface-elevated !bg-muted-foreground"
        position={Position.Bottom}
        type="source"
      />
    </div>
  );
}

export function ChildCanvasNode({ data, selected }: NodeProps<CanvasNodeData>) {
  return (
    <article
      className={cn(
        'min-w-56 max-w-72 rounded-xl border border-dashed bg-surface-base shadow-sm',
        selected ? 'border-accent-primary' : 'border-border'
      )}
      aria-label={`Child call ${data.name}`}
    >
      <Handle
        className="!size-2 !border-surface-base !bg-accent-primary"
        position={Position.Top}
        type="target"
      />
      <button
        className="nodrag w-full rounded-t-xl p-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-accent-primary"
        onClick={data.onActivate}
        type="button"
      >
        <span className="flex items-center gap-2 font-semibold text-accent-primary text-xs uppercase tracking-wide">
          <Workflow aria-hidden="true" className="size-3.5" />
          Child contract
        </span>
        <span className="mt-2 block font-mono text-foreground text-xs">{data.label}</span>
      </button>
      {data.childDocumentPath === undefined ? (
        <p className="border-border border-t px-3 py-2 text-muted-foreground text-xs">
          Contract stub
        </p>
      ) : (
        <button
          className="nodrag flex w-full items-center gap-1.5 rounded-b-xl border-border border-t px-3 py-2 text-accent-primary text-xs outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-accent-primary"
          onClick={data.onOpenChild}
          type="button"
        >
          <ExternalLink aria-hidden="true" className="size-3" />
          Open {data.childDocumentPath}
        </button>
      )}
      <Handle
        className="!size-2 !border-surface-base !bg-accent-primary"
        position={Position.Bottom}
        type="source"
      />
    </article>
  );
}
