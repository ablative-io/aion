import { useEffect, useRef, useState } from 'react';

import { Button } from '@/components/ui/button';
import { ACTION_IDS } from '@/lib/keybindings/actions';
import { useAction } from '@/lib/keybindings/context';

import { createCodeMirrorEditor } from '../editor-seam/codemirror';
import type { AuthoringEditor, HighlightSpan } from '../editor-seam/types';
import { mapDiagnostics, minimalTextDelta, statusForCheck } from '../lib/diagnostics';
import { authoringFacade, type CheckResult } from '../lib/facade';

export type EditorPaneProps = { path: string; initialSource: string };

type HighlightReply = { id: number; spans?: HighlightSpan[]; error?: string };

export function EditorPane({ path, initialSource }: EditorPaneProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<AuthoringEditor | null>(null);
  const highlightWorkerRef = useRef<Worker | null>(null);
  const sourceRef = useRef(initialSource);
  const savedSourceRef = useRef(initialSource);
  const requestIdRef = useRef(0);
  const [source, setSource] = useState(initialSource);
  const [dirty, setDirty] = useState(false);
  const [check, setCheck] = useState<CheckResult | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [working, setWorking] = useState<'format' | 'save' | null>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (host === null) return;
    const editor = createCodeMirrorEditor(host, initialSource);
    editorRef.current = editor;
    sourceRef.current = initialSource;
    savedSourceRef.current = initialSource;
    setSource(initialSource);
    setDirty(false);
    const unsubscribe = editor.onChange((change) => {
      sourceRef.current = change.content;
      setSource(change.content);
      setDirty(change.content !== savedSourceRef.current);
    });
    return () => {
      unsubscribe();
      editor.destroy();
      editorRef.current = null;
    };
  }, [initialSource]);

  useEffect(() => {
    const worker = new Worker(new URL('../lib/highlight.worker.ts', import.meta.url), {
      type: 'module',
    });
    highlightWorkerRef.current = worker;
    worker.onmessage = (event: MessageEvent<HighlightReply>) => {
      if (event.data.id === requestIdRef.current && event.data.spans !== undefined) {
        editorRef.current?.setHighlightSpans(event.data.spans);
      }
    };
    return () => {
      highlightWorkerRef.current = null;
      worker.terminate();
    };
  }, []);

  useEffect(() => {
    const id = ++requestIdRef.current;
    highlightWorkerRef.current?.postMessage({ id, source });
  }, [source]);

  useEffect(() => {
    let active = true;
    const timer = window.setTimeout(() => {
      void authoringFacade
        .check(source, path)
        .then((result) => {
          if (!active) return;
          setCheck(result);
          editorRef.current?.setDecorations(mapDiagnostics(source, result.diagnostics));
          setMessage(null);
        })
        .catch((error: unknown) => {
          if (active) setMessage(error instanceof Error ? error.message : 'Check failed');
        });
    }, 400);
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, [path, source]);

  async function save() {
    if (!dirty || working !== null) return;
    setWorking('save');
    setMessage(null);
    try {
      await authoringFacade.saveDocument(path, sourceRef.current);
      savedSourceRef.current = sourceRef.current;
      setDirty(false);
      setMessage('Saved');
    } catch (error) {
      setMessage(error instanceof Error ? error.message : 'Save failed');
    } finally {
      setWorking(null);
    }
  }

  useAction(ACTION_IDS.authoringSave, () => void save());

  async function format() {
    if (working !== null) return;
    setWorking('format');
    setMessage(null);
    try {
      const formatted = await authoringFacade.format(sourceRef.current);
      editorRef.current?.applyTextDelta(minimalTextDelta(sourceRef.current, formatted));
    } catch (error) {
      setMessage(error instanceof Error ? error.message : 'Format failed');
    } finally {
      setWorking(null);
    }
  }

  const status = check === null ? null : statusForCheck(check);
  const statusClass =
    status?.tone === 'green'
      ? 'text-success'
      : status?.tone === 'amber'
        ? 'text-warning'
        : 'text-destructive';

  return (
    <section className="flex min-w-0 flex-1 flex-col" aria-label={`Editing ${path}`}>
      <div className="flex flex-wrap items-center justify-between gap-3 border-border/70 border-b px-4 py-3">
        <div className="min-w-0">
          <h2 className="truncate font-medium text-sm">{path}</h2>
          <p className="text-muted-foreground text-xs">{dirty ? 'Unsaved changes' : 'Saved'}</p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            disabled={working !== null}
            onClick={() => void format()}
            size="sm"
            variant="outline"
          >
            {working === 'format' ? 'Formatting…' : 'Format'}
          </Button>
          <Button disabled={!dirty || working !== null} onClick={() => void save()} size="sm">
            {working === 'save' ? 'Saving…' : 'Save'}
          </Button>
        </div>
      </div>
      <div className="min-h-[32rem] flex-1 overflow-hidden" ref={hostRef} />
      <div className="flex min-h-10 items-center justify-between gap-4 border-border/70 border-t px-4 py-2 text-xs">
        <span className={statusClass}>{status?.label ?? 'Checking…'}</span>
        <span className="text-muted-foreground">
          {message ?? (check === null ? '' : `${check.steps} steps`)}
        </span>
      </div>
    </section>
  );
}
