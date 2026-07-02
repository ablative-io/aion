import { useQuery } from '@tanstack/react-query';
import { useMemo } from 'react';

import { useCapabilities } from '@/features/capabilities';
import { useNamespace } from '@/features/namespace';
import { createConfiguredApiClient } from '@/lib/config';

import {
  fetchPaletteEntities,
  type PaletteEntityClient,
  type PaletteEntityResult,
} from '../lib/entities';

const PALETTE_STALE_TIME_MS = 15_000;

export type UsePaletteEntitiesOptions = {
  /** Fetch only while the palette is open — no background polling. */
  enabled: boolean;
  /** Injected client (tests); the UI uses the configured live client. */
  client?: PaletteEntityClient | undefined;
};

/**
 * The palette's entity inventory, fetched from the LIVE REST surface when the
 * palette opens (selected namespace + runtime deploy capability scope the
 * sources). Kept warm briefly so re-opening the palette is instant without
 * going stale.
 */
export function usePaletteEntities(options: UsePaletteEntitiesOptions) {
  const { selectedNamespace } = useNamespace();
  const { deployGranted } = useCapabilities();
  const client = useMemo<PaletteEntityClient>(
    () => options.client ?? createConfiguredApiClient({ namespace: selectedNamespace }),
    [options.client, selectedNamespace]
  );

  return useQuery<PaletteEntityResult, Error>({
    queryKey: ['palette-entities', selectedNamespace, deployGranted],
    queryFn: () => fetchPaletteEntities(client, { namespace: selectedNamespace, deployGranted }),
    enabled: options.enabled,
    staleTime: PALETTE_STALE_TIME_MS,
  });
}
