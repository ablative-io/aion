import type { AwlDocument, CheckResult, EditResult, GestureOperation } from '../lib/facade';
import type { SemanticIndex } from '../lib/projection-types';
import { AuthoringCanvas } from './AuthoringCanvas';

export type AuthoringViewMode = 'text' | 'canvas' | 'split';

export function ProjectionPane({
  check,
  documents,
  onJumpToSpan,
  onGesture,
  onOpenDocument,
  path,
  selectedStep,
  semantic,
  viewMode,
}: {
  check: CheckResult | null;
  documents: readonly AwlDocument[];
  onJumpToSpan: (byteOffset: number) => void;
  onGesture: (operation: GestureOperation) => Promise<EditResult>;
  onOpenDocument: (path: string) => void;
  path: string;
  selectedStep: string | null;
  semantic: SemanticIndex | null;
  viewMode: AuthoringViewMode;
}) {
  if (viewMode === 'text') return null;
  if (semantic === null) {
    return (
      <div className="flex flex-1 items-center justify-center bg-surface-elevated text-muted-foreground text-sm">
        {check?.ok === false ? 'Fix source errors to create a projection' : 'Building projection…'}
      </div>
    );
  }
  return (
    <AuthoringCanvas
      diagnostics={check?.diagnostics ?? []}
      documents={documents}
      graph={semantic.graph}
      onGesture={onGesture}
      onJumpToSpan={onJumpToSpan}
      onOpenDocument={onOpenDocument}
      path={path}
      routeTargets={routeTargets(semantic)}
      selectedStep={selectedStep}
    />
  );
}

function routeTargets(semantic: SemanticIndex): string[] {
  const targets = semantic.graph.steps.map((step) => step.name);
  for (const entry of semantic.entries) {
    const declaration = entry.declaration;
    if (
      declaration?.kind === 'outcome' &&
      entry.span.start === declaration.span.start &&
      !targets.includes(declaration.name)
    ) {
      targets.push(declaration.name);
    }
  }
  return targets;
}
