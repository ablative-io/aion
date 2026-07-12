import { FileCode2 } from 'lucide-react';
import { AnimatePresence, motion, useReducedMotion } from 'motion/react';
import { useEffect, useState } from 'react';

import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { degradeToFade, SPRING_SIGNATURE } from '@/components/kit';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';

import { useAuthoringDocument, useAuthoringDocuments } from '../hooks/useAuthoringDocuments';
import { AuthoringWorkspaceNotConfiguredError } from '../lib/facade';
import { DocumentList } from './DocumentList';
import { EditorPane } from './EditorPane';

export function AuthoringView() {
  const documentsQuery = useAuthoringDocuments();
  const documents = documentsQuery.data ?? [];
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const reducedMotion = useReducedMotion() ?? false;
  const transition = degradeToFade(SPRING_SIGNATURE, reducedMotion);

  useEffect(() => {
    if (selectedPath === null && documents[0] !== undefined) setSelectedPath(documents[0].path);
    if (selectedPath !== null && documents.every((document) => document.path !== selectedPath)) {
      setSelectedPath(documents[0]?.path ?? null);
    }
  }, [documents, selectedPath]);

  const documentQuery = useAuthoringDocument(selectedPath);
  const unconfigured =
    documentsQuery.error instanceof AuthoringWorkspaceNotConfiguredError ||
    documentQuery.error instanceof AuthoringWorkspaceNotConfiguredError;

  if (unconfigured) {
    return (
      <section
        className="overflow-hidden rounded-xl border border-border bg-surface-elevated"
        aria-label="Authoring"
      >
        <header className="border-border border-b bg-surface-base px-4 py-3">
          <h1 className="font-semibold text-foreground text-lg">Authoring</h1>
          <p className="text-muted-foreground text-sm">AWL documents and deployment checks</p>
        </header>
        <DocumentList documents={[]} onSelect={() => undefined} selectedPath={null} unconfigured />
      </section>
    );
  }
  if (documentsQuery.isPending) return <LoadingSkeleton />;
  if (documentsQuery.isError) {
    return (
      <ErrorState
        message={documentsQuery.error.message}
        onRetry={() => void documentsQuery.refetch()}
        title="Documents unavailable"
      />
    );
  }

  return (
    <div className="overflow-hidden rounded-xl border border-border bg-surface-elevated shadow-sm">
      <header className="flex items-center justify-between gap-4 border-border border-b bg-surface-base px-4 py-3">
        <div>
          <h1 className="font-semibold text-foreground text-lg">Authoring</h1>
          <p className="text-muted-foreground text-sm">AWL documents and deployment checks</p>
        </div>
        <span className="rounded-full border border-border bg-surface-elevated px-2.5 py-1 font-mono text-muted-foreground text-xs">
          {documentCountLabel(documents.length)}
        </span>
      </header>
      <div className="flex min-h-[38rem] flex-col md:flex-row">
        <aside
          className="max-h-64 shrink-0 overflow-auto border-border border-b bg-surface-base md:max-h-none md:w-56 md:border-r md:border-b-0"
          aria-label="Documents"
        >
          <div className="sticky top-0 z-10 flex items-center justify-between border-border border-b bg-surface-base/95 px-3 py-2 backdrop-blur">
            <span className="font-medium text-muted-foreground text-xs uppercase tracking-wider">
              Documents
            </span>
            <span className="font-mono text-muted-foreground text-[0.6875rem]">AWL</span>
          </div>
          <DocumentList
            documents={documents}
            onSelect={setSelectedPath}
            selectedPath={selectedPath}
          />
        </aside>
        <div className="relative flex min-w-0 flex-1">
          <AnimatePresence initial={false} mode="wait">
            <motion.div
              animate={{ opacity: 1, y: 0 }}
              className="flex min-w-0 flex-1"
              exit={reducedMotion ? { opacity: 0 } : { opacity: 0, y: 3 }}
              initial={reducedMotion ? { opacity: 0 } : { opacity: 0, y: -3 }}
              key={selectedPath ?? 'empty'}
              transition={transition}
            >
              {selectedPath === null ? (
                <div className="m-auto w-full max-w-md p-6">
                  <EmptyState
                    description="Choose a document from the list to inspect and edit its source."
                    icon={<FileCode2 aria-hidden="true" className="size-5" />}
                    title="Select an AWL document"
                  />
                </div>
              ) : documentQuery.isPending ? (
                <LoadingSkeleton />
              ) : documentQuery.isError ? (
                <ErrorState
                  message={documentQuery.error.message}
                  onRetry={() => void documentQuery.refetch()}
                  title="Document unavailable"
                />
              ) : (
                <EditorPane initialSource={documentQuery.data} path={selectedPath} />
              )}
            </motion.div>
          </AnimatePresence>
        </div>
      </div>
    </div>
  );
}

function documentCountLabel(count: number) {
  return `${count} ${count === 1 ? 'document' : 'documents'}`;
}
