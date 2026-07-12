import { AlignLeft, Check, LoaderCircle, Save } from 'lucide-react';
import { AnimatePresence, motion } from 'motion/react';
import { useEffect, useRef, useState } from 'react';

import { StatusDot } from '@/components/kit';
import { Badge } from '@/components/ui';
import { Button } from '@/components/ui/button';
import { ACTION_IDS } from '@/lib/keybindings/actions';
import { useAction } from '@/lib/keybindings/context';
import { cn } from '@/lib/utils';

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
  const statusStyle = statusPresentation(status?.tone);
  const documentName = path.split('/').at(-1) ?? path;

  return (
    <section className="flex min-w-0 flex-1 flex-col" aria-label={`Editing ${path}`}>
      <header className="flex flex-wrap items-center justify-between gap-3 border-border border-b bg-surface-base px-4 py-3">
        <div className="flex min-w-0 items-center gap-3">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <h2 className="truncate font-semibold text-foreground text-sm">{documentName}</h2>
              <Badge className="shrink-0" variant={dirty ? 'secondary' : 'outline'}>
                {dirty ? 'Modified' : 'Saved'}
              </Badge>
            </div>
            <p className="mt-0.5 truncate font-mono text-muted-foreground text-[0.6875rem]">
              {path}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Button
            disabled={working !== null}
            onClick={() => void format()}
            size="sm"
            variant="outline"
          >
            <ActionIcon action="format" dirty={dirty} working={working} />
            {working === 'format' ? 'Formatting…' : 'Format'}
          </Button>
          <Button disabled={!dirty || working !== null} onClick={() => void save()} size="sm">
            <ActionIcon action="save" dirty={dirty} working={working} />
            {working === 'save' ? 'Saving…' : 'Save'}
          </Button>
        </div>
      </header>
      <div className="min-h-[32rem] flex-1 overflow-hidden" ref={hostRef} />
      <div className="flex min-h-11 items-center justify-between gap-4 border-border border-t bg-surface-base px-4 py-2 text-xs">
        <span
          className={cn(
            'inline-flex items-center gap-2 rounded-full border px-2.5 py-1 font-medium',
            statusStyle.className
          )}
        >
          <CheckStatusIcon checking={status === null} status={statusStyle.status} />
          {status?.label ?? 'Checking…'}
        </span>
        <AnimatePresence initial={false} mode="wait">
          <motion.span
            animate={{ opacity: 1, y: 0 }}
            className="truncate text-muted-foreground"
            exit={{ opacity: 0, y: 2 }}
            initial={{ opacity: 0, y: -2 }}
            key={message ?? check?.steps ?? 'checking'}
          >
            {message ?? (check === null ? 'Validating source' : `${check.steps} steps`)}
          </motion.span>
        </AnimatePresence>
      </div>
    </section>
  );
}

type WorkingAction = 'format' | 'save' | null;

function ActionIcon({
  action,
  dirty,
  working,
}: {
  action: Exclude<WorkingAction, null>;
  dirty: boolean;
  working: WorkingAction;
}) {
  if (working === action) {
    return <LoaderCircle aria-hidden="true" className="size-3.5 animate-spin" />;
  }
  if (action === 'format') return <AlignLeft aria-hidden="true" className="size-3.5" />;
  if (dirty) return <Save aria-hidden="true" className="size-3.5" />;
  return <Check aria-hidden="true" className="size-3.5" />;
}

function CheckStatusIcon({
  checking,
  status,
}: {
  checking: boolean;
  status: 'healthy' | 'running' | 'failed' | 'idle';
}) {
  if (checking) {
    return <LoaderCircle aria-hidden="true" className="size-3 animate-spin" />;
  }
  return <StatusDot className="size-1.5" status={status} />;
}

function statusPresentation(tone: 'green' | 'amber' | 'red' | undefined) {
  switch (tone) {
    case 'green':
      return {
        status: 'healthy' as const,
        className: 'border-success/30 bg-success/10 text-success',
      };
    case 'amber':
      return {
        status: 'running' as const,
        className: 'border-warning/30 bg-warning/10 text-warning',
      };
    case 'red':
      return {
        status: 'failed' as const,
        className: 'border-destructive/30 bg-destructive/10 text-destructive',
      };
    default:
      return {
        status: 'idle' as const,
        className: 'border-border bg-surface-elevated text-muted-foreground',
      };
  }
}
