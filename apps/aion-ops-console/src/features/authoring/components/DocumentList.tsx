import { FileCode2, FolderCog } from 'lucide-react';

import { EmptyState } from '@/components/EmptyState';
import { StatusDot } from '@/components/kit';
import { cn } from '@/lib/utils';
import type { AwlDocument } from '../lib/facade';

export type DocumentListProps = {
  documents: readonly AwlDocument[];
  selectedPath: string | null;
  unconfigured?: boolean;
  onSelect: (path: string) => void;
};

export function DocumentList({
  documents,
  selectedPath,
  unconfigured = false,
  onSelect,
}: DocumentListProps) {
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
  if (documents.length === 0) {
    return (
      <div className="p-3">
        <EmptyState
          description="Add an AWL file to the configured workspace to begin."
          icon={<FileCode2 aria-hidden="true" className="size-5" />}
          title="No AWL documents"
        />
      </div>
    );
  }
  return (
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
  );
}
