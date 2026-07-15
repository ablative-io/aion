import type { CSSProperties, ReactNode } from 'react';
import type { EdgeProps } from 'reactflow';

export type ParallelEdgeData = { siblingOffset: number };

type ParallelCanvasEdgeVisualProps = {
  id: string;
  sourceX: number;
  sourceY: number;
  targetX: number;
  targetY: number;
  siblingOffset: number;
  label?: ReactNode | undefined;
  markerEnd?: string | undefined;
  style?: CSSProperties | undefined;
};

/**
 * One root-canvas edge. Parallel endpoint siblings bend to opposite sides and
 * carry their labels with the bend, so outcome and failure routes stay
 * individually readable.
 */
export function ParallelCanvasEdgeVisual({
  id,
  sourceX,
  sourceY,
  targetX,
  targetY,
  siblingOffset,
  label,
  markerEnd,
  style,
}: ParallelCanvasEdgeVisualProps) {
  const direction = targetY >= sourceY ? 1 : -1;
  const controlDistance = Math.max(40, Math.abs(targetY - sourceY) / 2);
  const path = `M ${sourceX} ${sourceY} C ${sourceX + siblingOffset} ${
    sourceY + direction * controlDistance
  }, ${targetX + siblingOffset} ${targetY - direction * controlDistance}, ${targetX} ${targetY}`;
  const labelX = (sourceX + targetX) / 2 + siblingOffset;
  const labelY = (sourceY + targetY) / 2;
  return (
    <>
      <path
        className="react-flow__edge-path"
        d={path}
        data-edge-id={id}
        fill="none"
        markerEnd={markerEnd}
        style={style}
      />
      {label !== undefined && label !== null && (
        <text
          className="select-none fill-muted-foreground font-semibold text-[11px]"
          data-label-for={id}
          dominantBaseline="central"
          textAnchor="middle"
          x={labelX}
          y={labelY}
        >
          {label}
        </text>
      )}
    </>
  );
}

/** React Flow adapter for the parallel-edge visual. */
export function ParallelCanvasEdge({
  id,
  sourceX,
  sourceY,
  targetX,
  targetY,
  label,
  markerEnd,
  style,
  data,
}: EdgeProps<ParallelEdgeData>) {
  return (
    <ParallelCanvasEdgeVisual
      id={id}
      label={label}
      markerEnd={markerEnd}
      siblingOffset={data?.siblingOffset ?? 0}
      sourceX={sourceX}
      sourceY={sourceY}
      style={style}
      targetX={targetX}
      targetY={targetY}
    />
  );
}
