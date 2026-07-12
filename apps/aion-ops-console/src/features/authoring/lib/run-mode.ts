import type { GraphProjection } from './projection-types';

export function hasRevisionDrift(buffer: string, deployedSource: string): boolean {
  return buffer !== deployedSource;
}

export function activeStepForActivity(
  graph: GraphProjection,
  activityType: string | null | undefined
): string | null {
  if (activityType === null || activityType === undefined) return null;
  return graph.steps.find((step) => step.activities.includes(activityType))?.name ?? null;
}
