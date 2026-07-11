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
    return <p className="p-4 text-muted-foreground text-sm">authoring workspace not configured</p>;
  }
  if (documents.length === 0) {
    return <p className="p-4 text-muted-foreground text-sm">No AWL documents</p>;
  }
  return (
    <ul aria-label="AWL documents" className="divide-y divide-border/70">
      {documents.map((document) => (
        <li key={document.path}>
          <button
            className={
              document.path === selectedPath
                ? 'w-full bg-accent px-4 py-3 text-left font-medium text-foreground text-sm'
                : 'w-full px-4 py-3 text-left text-muted-foreground text-sm hover:bg-accent hover:text-foreground'
            }
            onClick={() => onSelect(document.path)}
            type="button"
          >
            <span className="block truncate">{document.name}</span>
            <span className="block truncate font-mono text-xs opacity-70">{document.path}</span>
          </button>
        </li>
      ))}
    </ul>
  );
}
