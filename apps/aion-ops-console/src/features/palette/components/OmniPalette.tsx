import { Command } from 'cmdk';
import type { CSSProperties } from 'react';
import { useState } from 'react';
import { useNavigate } from 'react-router';

import { useCapabilities } from '@/features/capabilities';
import { ACTION_IDS, useAction } from '@/lib/keybindings';
import { cn } from '@/lib/utils';

import { usePaletteEntities } from '../hooks/usePaletteEntities';
import type { PaletteEntityClient } from '../lib/entities';
import { buildPaletteGroups, type PaletteItem } from '../lib/groups';
import { cycleMode, PALETTE_MODE_LABELS, PALETTE_MODES, type PaletteMode } from '../lib/modes';
import { loadRecents, pushRecent } from '../lib/recents';
import { paletteVerbs } from '../lib/verbs';

export type OmniPaletteProps = {
  onClose: () => void;
  /** Injected entity client (tests); the UI uses the configured live client. */
  client?: PaletteEntityClient | undefined;
};

// Selection rides the action accent; the local var keeps the /12 alpha
// utility below readable.
const PALETTE_STYLE = {
  '--palette-accent': 'var(--accent-primary)',
} as CSSProperties;

const ITEM_CLASS = cn(
  'flex cursor-pointer items-center gap-3 rounded-md px-3 py-2 text-sm',
  'text-[var(--text-secondary)]',
  'data-[selected=true]:bg-(--palette-accent)/12 data-[selected=true]:text-[var(--text-primary)]'
);

const KIND_LABELS: Record<PaletteItem['kind'], string> = {
  workflow: 'workflow',
  namespace: 'namespace',
  package: 'package',
  worker: 'worker',
  action: 'action',
  recent: 'recent',
};

/**
 * The omni-palette (⌘K): fuzzy search over live entities (workflows,
 * namespaces, packages, workers) and verbs, grouped by kind, every result a
 * router deep link. Tab cycles modes; selections persist to a recents section.
 * Verbs navigate to the surface that performs the operation — the palette
 * never mutates anything itself. Keys arrive ONLY through the central registry
 * (view-scoped close/cycle actions live while the palette is mounted); cmdk's
 * own list traversal is internal input behavior, not a global listener.
 */
export function OmniPalette({ onClose, client }: OmniPaletteProps) {
  const navigate = useNavigate();
  const { deployGranted } = useCapabilities();
  const [mode, setMode] = useState<PaletteMode>('everything');
  const [query, setQuery] = useState('');
  const [recents, setRecents] = useState(() => loadRecents());
  const entityQuery = usePaletteEntities({ enabled: true, client });

  useAction(ACTION_IDS.paletteClose, onClose);
  useAction(ACTION_IDS.paletteCycleMode, () => setMode(cycleMode));

  const groups = buildPaletteGroups({
    mode,
    entities: entityQuery.data?.entities ?? [],
    verbs: paletteVerbs(deployGranted),
    recents,
    query,
  });

  function handleSelect(item: PaletteItem) {
    setRecents(
      pushRecent({
        id: item.id,
        kind: item.kind === 'recent' ? (item.keywords[0] ?? 'recent') : item.kind,
        title: item.title,
        subtitle: item.subtitle,
        href: item.href,
      })
    );
    onClose();
    navigate(item.href);
  }

  return (
    <div className="fixed inset-0 z-50" style={PALETTE_STYLE}>
      <button
        aria-label="Close command palette"
        className="absolute inset-0 h-full w-full cursor-default bg-black/40"
        onClick={onClose}
        type="button"
      />
      {/* Glass surface (summoned): translucent elevated bg + blur, per the design language. */}
      <div
        aria-label="Command palette"
        className={cn(
          'relative mx-auto mt-[15vh] w-full max-w-xl overflow-hidden rounded-xl',
          'border border-border bg-popover/85 shadow-2xl backdrop-blur-xl',
          'fade-in zoom-in-95 animate-in duration-150 motion-reduce:animate-none'
        )}
        role="dialog"
      >
        <Command label="Command palette" loop>
          <div className="flex items-center gap-1 border-border/70 border-b px-3 pt-3 pb-2">
            {PALETTE_MODES.map((candidate) => (
              <button
                className={cn(
                  'rounded-md px-2 py-1 text-xs uppercase tracking-[0.15em]',
                  candidate === mode
                    ? 'bg-(--palette-accent)/12 text-(--palette-accent)'
                    : 'text-[var(--text-muted)] hover:text-[var(--text-secondary)]'
                )}
                key={candidate}
                onClick={() => setMode(candidate)}
                type="button"
              >
                {PALETTE_MODE_LABELS[candidate]}
              </button>
            ))}
            <span className="ml-auto text-[var(--text-muted)] text-xs">Tab to switch</span>
          </div>
          <Command.Input
            autoFocus
            className="w-full border-border/70 border-b bg-transparent px-4 py-3 text-[var(--text-primary)] text-sm outline-none placeholder:text-[var(--text-muted)]"
            onValueChange={setQuery}
            placeholder="Search workflows, namespaces, packages, actions…"
            value={query}
          />
          <Command.List className="max-h-[50vh] overflow-y-auto p-2">
            {entityQuery.isLoading ? (
              <Command.Loading>
                <p className="px-3 py-2 text-[var(--text-muted)] text-sm">Loading entities…</p>
              </Command.Loading>
            ) : null}
            <Command.Empty>
              <p className="px-3 py-6 text-center text-[var(--text-muted)] text-sm">
                No results for this mode.
              </p>
            </Command.Empty>
            {groups.map((group) => (
              <Command.Group
                className="[&_[cmdk-group-heading]]:px-3 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-[10px] [&_[cmdk-group-heading]]:text-[var(--text-muted)] [&_[cmdk-group-heading]]:uppercase [&_[cmdk-group-heading]]:tracking-[0.15em]"
                heading={group.heading}
                key={group.heading}
              >
                {group.items.map((item) => (
                  <Command.Item
                    className={ITEM_CLASS}
                    key={item.id}
                    keywords={[...item.keywords, item.subtitle]}
                    onSelect={() => handleSelect(item)}
                    value={`${item.title} ${item.id}`}
                  >
                    <span className="truncate font-medium">{item.title}</span>
                    <span className="min-w-0 truncate text-[var(--text-muted)] text-xs">
                      {item.subtitle}
                    </span>
                    <span className="ml-auto shrink-0 rounded border border-border px-1.5 py-0.5 text-[10px] text-[var(--text-muted)] uppercase tracking-[0.1em]">
                      {KIND_LABELS[item.kind]}
                    </span>
                  </Command.Item>
                ))}
              </Command.Group>
            ))}
          </Command.List>
          <PaletteFooter errors={entityQuery.data?.errors ?? []} fetchError={entityQuery.error} />
        </Command>
      </div>
    </div>
  );
}

function PaletteFooter({ errors, fetchError }: { errors: string[]; fetchError: Error | null }) {
  const problems = [...errors, ...(fetchError === null ? [] : [fetchError.message])];

  return (
    <div className="border-border/70 border-t px-4 py-2">
      {problems.map((problem) => (
        <p className="py-0.5 text-[var(--destructive)] text-xs" key={problem}>
          {problem}
        </p>
      ))}
      <p className="text-[10px] text-[var(--text-muted)] tracking-[0.05em]">
        ↑↓ navigate · ↵ open · Tab mode · Esc close
      </p>
    </div>
  );
}
