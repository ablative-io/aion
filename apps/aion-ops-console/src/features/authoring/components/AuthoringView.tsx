import { useEffect, useState } from 'react';

import { ErrorState } from '@/components/ErrorState';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';

import { useAuthoringDocument, useAuthoringDocuments } from '../hooks/useAuthoringDocuments';
import { AuthoringWorkspaceNotConfiguredError } from '../lib/facade';
import { DocumentList } from './DocumentList';
import { EditorPane } from './EditorPane';

export function AuthoringView() {
  const documentsQuery = useAuthoringDocuments();
  const documents = documentsQuery.data ?? [];
  const [selectedPath, setSelectedPath] = useState<string | null>(null);

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
      <section className="rounded-lg border border-border bg-card" aria-label="Authoring">
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
    <div className="overflow-hidden rounded-lg border border-border bg-card">
      <header className="border-border/70 border-b px-5 py-4">
        <h1 className="font-semibold text-lg">Authoring</h1>
        <p className="text-muted-foreground text-sm">
          Edit, check, format, and save AWL documents.
        </p>
      </header>
      <div className="flex min-h-[38rem]">
        <aside className="w-64 shrink-0 border-border/70 border-r" aria-label="Documents">
          <DocumentList
            documents={documents}
            onSelect={setSelectedPath}
            selectedPath={selectedPath}
          />
        </aside>
        {selectedPath === null ? (
          <p className="m-auto text-muted-foreground text-sm">Select an AWL document</p>
        ) : documentQuery.isPending ? (
          <LoadingSkeleton />
        ) : documentQuery.isError ? (
          <ErrorState
            message={documentQuery.error.message}
            onRetry={() => void documentQuery.refetch()}
            title="Document unavailable"
          />
        ) : (
          <EditorPane initialSource={documentQuery.data} key={selectedPath} path={selectedPath} />
        )}
      </div>
    </div>
  );
}
