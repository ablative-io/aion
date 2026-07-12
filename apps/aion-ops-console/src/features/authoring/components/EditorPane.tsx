import { AlignLeft, Check, Columns2, FileText, LoaderCircle, Save, Workflow } from 'lucide-react';
import { AnimatePresence, motion } from 'motion/react';
import { useCallback, useEffect, useRef, useState } from 'react';

import { StatusDot } from '@/components/kit';
import { Badge } from '@/components/ui';
import { Button } from '@/components/ui/button';
import { ACTION_IDS } from '@/lib/keybindings/actions';
import { useAction } from '@/lib/keybindings/context';
import { cn } from '@/lib/utils';

import { createCodeMirrorEditor } from '../editor-seam/codemirror';
import type { AuthoringEditor, HighlightSpan } from '../editor-seam/types';
import { mapDiagnostics, minimalTextDelta, statusForCheck } from '../lib/diagnostics';
import {
  type AwlDocument,
  authoringFacade,
  type CheckResult,
  type EditResult,
  type GestureOperation,
} from '../lib/facade';
import { applyGestureThroughSeam } from '../lib/gestures';
import { byteOffsetToUtf16, stepAtPosition } from '../lib/projection';
import type { SemanticIndex } from '../lib/projection-types';
import { type AuthoringViewMode, ProjectionPane } from './ProjectionPane';

export type EditorPaneProps = {
  path: string;
  initialSource: string;
  documents: readonly AwlDocument[];
  onOpenDocument: (path: string) => void;
};

type ViewMode = AuthoringViewMode;

type HighlightReply = { id: number; spans?: HighlightSpan[]; error?: string };

export function EditorPane({ path, initialSource, documents, onOpenDocument }: EditorPaneProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<AuthoringEditor | null>(null);
  const semanticRef = useRef<SemanticIndex | null>(null);
  const cursorPositionRef = useRef(0);
  const highlightWorkerRef = useRef<Worker | null>(null);
  const sourceRef = useRef(initialSource);
  const savedSourceRef = useRef(initialSource);
  const requestIdRef = useRef(0);
  const [source, setSource] = useState(initialSource);
  const [dirty, setDirty] = useState(false);
  const [check, setCheck] = useState<CheckResult | null>(null);
  const [semantic, setSemantic] = useState<SemanticIndex | null>(null);
  const [selectedStep, setSelectedStep] = useState<string | null>(null);
  const [viewMode, setViewMode] = useState<ViewMode>('text');
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
    const unsubscribeCursor = editor.onCursorChange(({ position }) => {
      cursorPositionRef.current = position;
      const graph = semanticRef.current?.graph;
      setSelectedStep(
        graph === undefined
          ? null
          : (stepAtPosition(graph, sourceRef.current, position)?.name ?? null)
      );
    });
    return () => {
      unsubscribe();
      unsubscribeCursor();
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
          if (result.semantic !== null) {
            semanticRef.current = result.semantic;
            setSemantic(result.semantic);
            setSelectedStep(
              stepAtPosition(result.semantic.graph, source, cursorPositionRef.current)?.name ?? null
            );
          }
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
  useAction(ACTION_IDS.authoringViewText, () => setViewMode('text'));
  useAction(ACTION_IDS.authoringViewCanvas, () => setViewMode('canvas'));
  useAction(ACTION_IDS.authoringViewSplit, () => setViewMode('split'));

  const jumpToSpan = useCallback((byteOffset: number) => {
    setViewMode((current) => (current === 'canvas' ? 'text' : current));
    const editor = editorRef.current;
    editor?.setCursor(byteOffsetToUtf16(sourceRef.current, byteOffset));
    window.requestAnimationFrame(() => editor?.focus());
  }, []);

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

  const applyGesture = useCallback(async (operation: GestureOperation): Promise<EditResult> => {
    setMessage(null);
    try {
      const editor = editorRef.current;
      if (editor === null) throw new Error('Editor is not available');
      // This is the architectural seam: every server-printed gesture reaches
      // CodeMirror in exactly one applyTextDelta call, hence one undo unit.
      const result = await applyGestureThroughSeam(editor, authoringFacade, operation);
      setMessage('Canvas gesture applied');
      return result;
    } catch (error) {
      setMessage(error instanceof Error ? error.message : 'Canvas gesture failed');
      throw error;
    }
  }, []);

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
          <fieldset
            className="flex rounded-lg border border-border bg-surface-elevated p-0.5"
            aria-label="Authoring view"
          >
            <ViewButton
              icon={<FileText className="size-3.5" />}
              label="Text"
              mode="text"
              selected={viewMode}
              onSelect={setViewMode}
            />
            <ViewButton
              icon={<Workflow className="size-3.5" />}
              label="Canvas"
              mode="canvas"
              selected={viewMode}
              onSelect={setViewMode}
            />
            <ViewButton
              icon={<Columns2 className="size-3.5" />}
              label="Split"
              mode="split"
              selected={viewMode}
              onSelect={setViewMode}
            />
          </fieldset>
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
      <div className="flex min-h-[32rem] min-w-0 flex-1 overflow-hidden">
        <div
          className={cn(
            'min-h-[32rem] min-w-0 overflow-hidden',
            viewMode === 'canvas' ? 'hidden' : 'flex-1',
            viewMode === 'split' && 'border-border border-r'
          )}
          ref={hostRef}
        />
        <ProjectionPane
          check={check}
          documents={documents}
          onJumpToSpan={jumpToSpan}
          onGesture={applyGesture}
          onOpenDocument={onOpenDocument}
          path={path}
          selectedStep={selectedStep}
          semantic={semantic}
          viewMode={viewMode}
        />
      </div>
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

function ViewButton({
  icon,
  label,
  mode,
  selected,
  onSelect,
}: {
  icon: React.ReactNode;
  label: string;
  mode: ViewMode;
  selected: ViewMode;
  onSelect: (mode: ViewMode) => void;
}) {
  return (
    <button
      aria-label={`${label} view`}
      aria-pressed={selected === mode}
      className={cn(
        'flex items-center gap-1.5 rounded-md px-2 py-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-accent-primary',
        selected === mode
          ? 'bg-accent text-foreground'
          : 'text-muted-foreground hover:text-foreground'
      )}
      onClick={() => onSelect(mode)}
      type="button"
    >
      {icon}
      <span className="hidden lg:inline">{label}</span>
    </button>
  );
}

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
