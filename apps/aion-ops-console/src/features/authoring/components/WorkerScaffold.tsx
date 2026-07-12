import { Copy, Download } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';

import { authoringFacade, type ScaffoldFiles, type WorkerRuntime } from '../lib/facade';

const picker =
  'rounded-md border border-border bg-surface-base px-2 py-1.5 text-foreground text-xs outline-none focus:ring-2 focus:ring-accent-primary';

export function WorkerScaffold({ source, worker }: { source: string; worker: string }) {
  const [runtime, setRuntime] = useState<WorkerRuntime>('shell');
  const [files, setFiles] = useState<ScaffoldFiles | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  const generate = async () => {
    setWorking(true);
    setError(null);
    try {
      setFiles(await authoringFacade.scaffold(source, worker, runtime));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Worker scaffold refused');
    } finally {
      setWorking(false);
    }
  };

  return (
    <div className="mt-3 border-border-subtle border-t pt-3">
      <div className="flex items-center gap-2">
        <select
          aria-label={`Runtime for ${worker}`}
          className={picker}
          onChange={(event) => setRuntime(event.target.value as WorkerRuntime)}
          value={runtime}
        >
          <option value="shell">Shell</option>
          <option value="rust">Rust</option>
        </select>
        <Button disabled={working} onClick={() => void generate()} size="sm" variant="outline">
          {working ? 'Generating…' : 'Generate worker'}
        </Button>
      </div>
      {error !== null && (
        <p className="mt-2 text-destructive text-xs" role="alert">
          {error}
        </p>
      )}
      {files !== null && (
        <div className="mt-3 space-y-3">
          {Object.entries(files).map(([path, content]) => (
            <article className="overflow-hidden rounded-md border border-border" key={path}>
              <header className="flex items-center justify-between bg-surface-elevated px-2 py-1.5">
                <code className="text-foreground text-xs">{path}</code>
                <span className="flex gap-1">
                  <button
                    aria-label={`Copy ${path}`}
                    className="rounded p-1 text-muted-foreground hover:text-foreground"
                    onClick={() => void navigator.clipboard.writeText(content)}
                    type="button"
                  >
                    <Copy className="size-3.5" />
                  </button>
                  <a
                    aria-label={`Download ${path}`}
                    className="rounded p-1 text-muted-foreground hover:text-foreground"
                    download={path.split('/').at(-1)}
                    href={fileUrl(content)}
                  >
                    <Download className="size-3.5" />
                  </a>
                </span>
              </header>
              <pre className="max-h-48 overflow-auto whitespace-pre p-2 text-foreground text-xs">
                <code>{content}</code>
              </pre>
            </article>
          ))}
        </div>
      )}
    </div>
  );
}

function fileUrl(content: string) {
  return `data:text/plain;charset=utf-8,${encodeURIComponent(content)}`;
}
