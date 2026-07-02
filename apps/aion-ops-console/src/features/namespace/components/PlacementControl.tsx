import { useState } from 'react';

import {
  Badge,
  Button,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui';
import { cn } from '@/lib/utils';
import type { Namespace, NamespacePlacementWire } from '@/types';

/**
 * Placement render + edit control for one namespace row (Control-Plane Phase 2,
 * P2-I2).
 *
 * Renders the current durable placement (from the REST record, kept live by the
 * `NamespacePlacementChanged` socket fold) and, when expanded, an operator editor
 * that issues `PUT /namespaces/{name}/placement` via `onSetPlacement`. On success
 * the server emits `NamespacePlacementChanged`, which the panel folds — so this
 * control never optimistically writes the row; the socket is the source of truth.
 *
 * The isolation meaning is shown honestly: Pinned = hard/isolated (workers MUST be
 * on the label set, enforced as of #164), Prefer = soft/spill, Unplaced = default.
 */

/** The three placement kinds, as the stable wire tags. */
const PLACEMENT_KINDS = ['unplaced', 'prefer', 'pinned'] as const;
type PlacementKind = (typeof PLACEMENT_KINDS)[number];

/** Operator-facing label + isolation semantics for each kind. */
const KIND_META: Record<PlacementKind, { label: string; meaning: string; badge: string }> = {
  unplaced: {
    label: 'Unplaced',
    meaning: 'Default — any live worker.',
    badge: 'text-muted-foreground',
  },
  prefer: {
    label: 'Prefer (soft)',
    meaning: 'Prefer these nodes; spill to any live worker when absent.',
    badge: 'border-live/40 bg-live-glow text-live',
  },
  pinned: {
    label: 'Pinned (hard)',
    meaning: 'Require these nodes; workers must be on the label set (isolated).',
    badge: 'border-special/40 bg-special-glow text-special',
  },
};

function isPlacementKind(value: string): value is PlacementKind {
  return (PLACEMENT_KINDS as readonly string[]).includes(value);
}

export type SetPlacement = (
  namespace: Namespace,
  placement: NamespacePlacementWire
) => Promise<void>;

export type PlacementControlProps = {
  namespace: Namespace;
  placement: NamespacePlacementWire;
  onSetPlacement: SetPlacement;
};

export function PlacementControl({ namespace, placement, onSetPlacement }: PlacementControlProps) {
  const [editing, setEditing] = useState(false);

  if (!editing) {
    return (
      <div className="flex items-center gap-2">
        <PlacementSummary placement={placement} />
        <Button
          variant="ghost"
          size="sm"
          onClick={() => setEditing(true)}
          aria-label={`Edit placement for ${namespace}`}
        >
          Edit
        </Button>
      </div>
    );
  }

  return (
    <PlacementEditor
      namespace={namespace}
      placement={placement}
      onSetPlacement={onSetPlacement}
      onDone={() => setEditing(false)}
    />
  );
}

/** Read-only badge + node list for the current placement. */
function PlacementSummary({ placement }: { placement: NamespacePlacementWire }) {
  const kind: PlacementKind = isPlacementKind(placement.kind) ? placement.kind : 'unplaced';
  const meta = KIND_META[kind];

  return (
    <span className="flex items-center gap-2" data-placement-kind={placement.kind}>
      <Badge variant="outline" className={cn('w-fit', meta.badge)} title={meta.meaning}>
        {isPlacementKind(placement.kind) ? meta.label : placement.kind}
      </Badge>
      {placement.nodes.length > 0 ? (
        <span className="font-mono text-muted-foreground text-xs">
          {placement.nodes.join(', ')}
        </span>
      ) : null}
    </span>
  );
}

type PlacementEditorProps = PlacementControlProps & { onDone: () => void };

function PlacementEditor({ namespace, placement, onSetPlacement, onDone }: PlacementEditorProps) {
  const [kind, setKind] = useState<PlacementKind>(
    isPlacementKind(placement.kind) ? placement.kind : 'unplaced'
  );
  const [nodesText, setNodesText] = useState(placement.nodes.join(', '));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const nodes = parseNodes(nodesText);
  const needsNodes = kind === 'prefer' || kind === 'pinned';
  const invalid = needsNodes && nodes.length === 0;

  const apply = (): void => {
    if (invalid) {
      setError('Prefer/Pinned require at least one node label.');
      return;
    }
    setBusy(true);
    setError(null);
    onSetPlacement(namespace, { kind, nodes: needsNodes ? nodes : [] })
      .then(() => onDone())
      .catch((cause: unknown) => {
        setError(cause instanceof Error ? cause.message : 'Failed to set placement.');
      })
      .finally(() => setBusy(false));
  };

  return (
    <fieldset
      className="flex flex-col gap-2 border-0 p-0"
      aria-label={`Set placement for ${namespace}`}
    >
      <div className="flex items-center gap-2">
        <Select value={kind} onValueChange={(next) => setKind(next as PlacementKind)}>
          <SelectTrigger className="w-40" aria-label="Placement kind">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {PLACEMENT_KINDS.map((option) => (
              <SelectItem key={option} value={option}>
                {KIND_META[option].label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {needsNodes ? (
          <input
            type="text"
            value={nodesText}
            onChange={(event) => setNodesText(event.target.value)}
            placeholder="node-a, node-b"
            aria-label="Node labels (comma separated)"
            className={cn(
              'h-9 rounded-lg border bg-transparent px-3 font-mono text-sm',
              'border-border text-foreground',
              'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring'
            )}
          />
        ) : null}
        <Button variant="default" size="sm" onClick={apply} disabled={busy || invalid}>
          {busy ? 'Setting…' : 'Apply'}
        </Button>
        <Button variant="ghost" size="sm" onClick={onDone} disabled={busy}>
          Cancel
        </Button>
      </div>
      <p className="text-muted-foreground text-xs">{KIND_META[kind].meaning}</p>
      {error === null ? null : <p className="text-destructive text-xs">{error}</p>}
    </fieldset>
  );
}

/** Split a comma/whitespace-separated label string into a trimmed, non-empty set. */
function parseNodes(text: string): string[] {
  return text
    .split(',')
    .map((label) => label.trim())
    .filter((label) => label.length > 0);
}
