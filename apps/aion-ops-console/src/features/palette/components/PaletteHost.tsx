import { useState } from 'react';

import { ACTION_IDS, useAction, useKeybindings } from '@/lib/keybindings';

import { OmniPalette } from './OmniPalette';

/**
 * Owns the palette's open state and its trigger chip. The chip doubles as the
 * shell's keyboard hint (⌘K on macOS, Sup+K on Linux — formatted from the
 * registry so a remap shows the operator's real binding). The open action is
 * registered through the central registry; there is no local key listener.
 */
export function PaletteHost() {
  const [open, setOpen] = useState(false);
  const registry = useKeybindings();

  useAction(ACTION_IDS.paletteOpen, () => setOpen((current) => !current));

  return (
    <>
      <button
        className="flex items-center gap-2 rounded-md border border-border px-2 py-1 text-[var(--text-muted)] text-xs transition-colors hover:text-[var(--text-secondary)]"
        onClick={() => setOpen(true)}
        type="button"
      >
        <span className="rounded border border-border bg-secondary px-1 font-mono text-[10px]">
          {registry.formatFor(ACTION_IDS.paletteOpen)}
        </span>
        command palette
      </button>
      {open ? <OmniPalette onClose={() => setOpen(false)} /> : null}
    </>
  );
}
