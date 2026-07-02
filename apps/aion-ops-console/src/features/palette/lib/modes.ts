/**
 * Palette modes, cycled with Tab: `everything` searches entities AND verbs,
 * `workflows` narrows to workflow entities, `actions` narrows to verbs.
 */
export const PALETTE_MODES = ['everything', 'workflows', 'actions'] as const;

export type PaletteMode = (typeof PALETTE_MODES)[number];

export const PALETTE_MODE_LABELS: Record<PaletteMode, string> = {
  everything: 'Everything',
  workflows: 'Workflows',
  actions: 'Actions',
};

/** The next mode in the Tab cycle (wraps). */
export function cycleMode(mode: PaletteMode): PaletteMode {
  const index = PALETTE_MODES.indexOf(mode);
  return PALETTE_MODES[(index + 1) % PALETTE_MODES.length] ?? 'everything';
}
