import { useVirtualizer } from '@tanstack/react-virtual';
import { useEffect, useMemo, useRef } from 'react';

import type { WorkflowId } from '@/types';

import type { LaneTreeRow } from './laneTree';

/** Owns the bounded row viewport and keeps URL/bar selection inside its mounted window. */
export function useVirtualizedLaneRows(
  rows: readonly LaneTreeRow[],
  selectedWorkflowId: WorkflowId | null,
  selectedSequence: number | null
) {
  const rowScrollRef = useRef<HTMLDivElement | null>(null);
  const rowVirtualizer = useVirtualizer({
    count: rows.length,
    estimateSize: () => 36,
    getItemKey: (index) => rows[index]?.id ?? index,
    getScrollElement: () => rowScrollRef.current,
    initialRect: { width: 1024, height: 432 },
    overscan: 6,
  });
  const selectedRowIndex = useMemo(
    () =>
      rows.findIndex(
        (row) =>
          row.kind === 'lane' &&
          row.workflowId === selectedWorkflowId &&
          row.lane.bars.some((bar) => bar.sequence === selectedSequence)
      ),
    [rows, selectedSequence, selectedWorkflowId]
  );

  useEffect(() => {
    if (selectedRowIndex >= 0) {
      rowVirtualizer.scrollToIndex(selectedRowIndex, { align: 'auto' });
    }
  }, [rowVirtualizer, selectedRowIndex]);

  return { rowScrollRef, rowVirtualizer };
}
