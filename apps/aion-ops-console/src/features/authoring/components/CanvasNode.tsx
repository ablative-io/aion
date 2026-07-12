import { ExternalLink, GitFork, Repeat2, Timer, Workflow } from 'lucide-react';
import { Handle, type NodeProps, Position } from 'reactflow';

import { cn } from '@/lib/utils';

export type CanvasNodeData = {
  kind: 'step' | 'child';
  name: string;
  label: string;
  markers?: { looped: boolean; forked: boolean; waits: boolean };
  diagnostic?: 'primary' | 'cascade' | undefined;
  childDocumentPath?: string | undefined;
  onActivate: () => void;
  onOpenChild?: (() => void) | undefined;
};

export function StepCanvasNode({ data, selected }: NodeProps<CanvasNodeData>) {
  return (
    <article
      className={cn(
        'min-w-52 max-w-64 rounded-xl border bg-surface-elevated shadow-sm transition-[border-color,box-shadow]',
        selected && 'border-accent-primary shadow-md',
        !selected && data.diagnostic === undefined && 'border-border',
        data.diagnostic === 'primary' && 'border-destructive bg-destructive/10',
        data.diagnostic === 'cascade' && 'border-muted-foreground/40 opacity-70'
      )}
      aria-label={`Step ${data.name}`}
    >
      <Handle
        className="!size-2 !border-surface-elevated !bg-muted-foreground"
        position={Position.Top}
        type="target"
      />
      <button
        className="nodrag w-full rounded-xl p-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-accent-primary focus-visible:ring-offset-2 focus-visible:ring-offset-surface-base"
        onClick={data.onActivate}
        type="button"
      >
        <span className="flex items-center justify-between gap-2">
          <span className="truncate font-mono font-semibold text-foreground text-xs">
            {data.name}
          </span>
          <MarkerBadges markers={data.markers} />
        </span>
        <span className="mt-2 block text-foreground text-sm leading-snug">
          {data.label || 'Undocumented step'}
        </span>
        {data.diagnostic !== undefined && (
          <span
            className={cn(
              'mt-2 block text-xs',
              data.diagnostic === 'primary' ? 'text-destructive' : 'text-muted-foreground'
            )}
          >
            {data.diagnostic === 'primary' ? 'Primary error' : 'Downstream cascade'}
          </span>
        )}
      </button>
      <Handle
        className="!size-2 !border-surface-elevated !bg-muted-foreground"
        position={Position.Bottom}
        type="source"
      />
    </article>
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

function MarkerBadges({ markers }: { markers: CanvasNodeData['markers'] }) {
  if (markers === undefined) return null;
  return (
    <span className="flex items-center gap-1 text-muted-foreground">
      {markers.looped && <Repeat2 aria-label="Loop" className="size-3.5" />}
      {markers.forked && <GitFork aria-label="Fork" className="size-3.5" />}
      {markers.waits && <Timer aria-label="Wait" className="size-3.5" />}
    </span>
  );
}
