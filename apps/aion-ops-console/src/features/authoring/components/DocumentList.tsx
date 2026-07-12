import { FileCode2, FolderCog, Plus } from 'lucide-react';
import { useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { StatusDot } from '@/components/kit';
import { cn } from '@/lib/utils';
import type { AwlDocument } from '../lib/facade';

export type DocumentListProps = {
  documents: readonly AwlDocument[];
  selectedPath: string | null;
  unconfigured?: boolean;
  onSelect: (path: string) => void;
  onCreate?: (name: string) => Promise<void>;
};

export function DocumentList({
  documents,
  selectedPath,
  unconfigured = false,
  onSelect,
  onCreate,
}: DocumentListProps) {
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  if (unconfigured) {
    return (
      <div className="p-3">
        <EmptyState
          description="authoring workspace not configured"
          icon={<FolderCog aria-hidden="true" className="size-5" />}
          title="Workspace unavailable"
        />
      </div>
    );
  }
  return (
    <div>
      {onCreate !== undefined && (
        <form
          className="space-y-2 border-border border-b p-2"
          onSubmit={(event) => {
            event.preventDefault();
            if (!/^[a-z_][a-z0-9_]*$/.test(name)) {
              setError('Use lowercase letters, digits, and underscores.');
              return;
            }
            setCreating(true);
            setError(null);
            void onCreate(name)
              .then(() => setName(''))
              .catch((reason: unknown) =>
                setError(reason instanceof Error ? reason.message : 'Create failed')
              )
              .finally(() => setCreating(false));
          }}
        >
          <label
            className="block font-medium text-muted-foreground text-xs"
            htmlFor="new-workflow-name"
          >
            New workflow
          </label>
          <div className="flex gap-1">
            <input
              id="new-workflow-name"
              className="min-w-0 flex-1 rounded-md border border-border bg-surface-elevated px-2 py-1.5 text-foreground text-xs outline-none focus:ring-2 focus:ring-accent-primary"
              onChange={(event) => setName(event.target.value)}
              placeholder="order_flow"
              value={name}
            />
            <button
              aria-label="Create workflow"
              className="rounded-md bg-primary p-2 text-primary-foreground disabled:opacity-50"
              disabled={creating || name.length === 0}
              type="submit"
            >
              <Plus className="size-3.5" />
            </button>
          </div>
          {error !== null && (
            <p className="text-destructive text-xs" role="alert">
              {error}
            </p>
          )}
        </form>
      )}
      {documents.length === 0 ? (
        <p className="p-4 text-center text-muted-foreground text-xs">Create a workflow to begin.</p>
      ) : (
        <ul aria-label="AWL documents" className="space-y-1 p-2">
          {documents.map((document) => (
            <li key={document.path}>
              <button
                aria-current={document.path === selectedPath ? 'true' : undefined}
                className={cn(
                  'group w-full rounded-md border border-transparent px-3 py-2.5 text-left transition-colors',
                  'hover:border-border-subtle hover:bg-surface-hover',
                  document.path === selectedPath &&
                    'border-border bg-surface-elevated shadow-sm hover:bg-surface-elevated'
                )}
                onClick={() => onSelect(document.path)}
                type="button"
              >
                <span className="flex items-center gap-2">
                  <FileCode2
                    aria-hidden="true"
                    className={cn(
                      'size-3.5 shrink-0 text-muted-foreground transition-colors',
                      document.path === selectedPath && 'text-primary'
                    )}
                  />
                  <span className="min-w-0 flex-1 truncate font-medium text-foreground text-sm">
                    {document.name}
                  </span>
                </span>
                <span className="mt-1.5 flex items-center justify-between gap-2 pl-5.5">
                  <span className="min-w-0 truncate font-mono text-muted-foreground text-[0.6875rem]">
                    {document.path}
                  </span>
                  <span className="inline-flex shrink-0 items-center gap-1.5 rounded-full bg-success/10 px-2 py-0.5 font-medium text-[0.625rem] text-success">
                    <StatusDot className="size-1.5" status="healthy" />
                    deploys green
                  </span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
