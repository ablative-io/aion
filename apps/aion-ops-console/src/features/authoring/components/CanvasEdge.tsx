import type { CSSProperties, ReactNode } from 'react';
import type { EdgeProps } from 'reactflow';

import { EDGE_GUTTER_CLEARANCE, edgeLaneSpread, lateralEdgePath } from '../lib/canvas-layout';

export type ParallelEdgeData = {
  siblingOffset: number;
  back: boolean;
  selfLoop: boolean;
  graphLeft: number;
  graphRight: number;
};

type ParallelCanvasEdgeVisualProps = {
  id: string;
  sourceX: number;
  sourceY: number;
  targetX: number;
  targetY: number;
  siblingOffset: number;
  back: boolean;
  selfLoop: boolean;
  graphLeft: number;
  graphRight: number;
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
  back,
  selfLoop,
  graphLeft,
  graphRight,
  label,
  markerEnd,
  style,
}: ParallelCanvasEdgeVisualProps) {
  const gutterDistance = EDGE_GUTTER_CLEARANCE + edgeLaneSpread(siblingOffset);
  const labelY = (sourceY + targetY) / 2;
  let path: string;
  let labelX: number;
  if (selfLoop) {
    labelX = graphRight + gutterDistance;
    path = lateralEdgePath({ x: sourceX, y: sourceY }, { x: targetX, y: targetY }, labelX);
  } else if (back) {
    labelX = graphLeft - gutterDistance;
    path = lateralEdgePath({ x: sourceX, y: sourceY }, { x: targetX, y: targetY }, labelX);
  } else {
    const direction = targetY >= sourceY ? 1 : -1;
    const controlDistance = Math.max(40, Math.abs(targetY - sourceY) / 2);
    path = `M ${sourceX} ${sourceY} C ${sourceX + siblingOffset} ${
      sourceY + direction * controlDistance
    }, ${targetX + siblingOffset} ${targetY - direction * controlDistance}, ${targetX} ${targetY}`;
    labelX = (sourceX + targetX) / 2 + siblingOffset;
  }
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
      back={data?.back ?? false}
      graphLeft={data?.graphLeft ?? Math.min(sourceX, targetX)}
      graphRight={data?.graphRight ?? Math.max(sourceX, targetX)}
      id={id}
      label={label}
      markerEnd={markerEnd}
      selfLoop={data?.selfLoop ?? false}
      siblingOffset={data?.siblingOffset ?? 0}
      sourceX={sourceX}
      sourceY={sourceY}
      style={style}
      targetX={targetX}
      targetY={targetY}
    />
  );
}
