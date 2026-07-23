import { useState } from 'react';

import { Badge, Button } from '@/components/ui';

import type { TranscriptToolEntry, TranscriptToolStreamEntry } from '../lib/entries';
import { type DecodedToolResult, isRecord, prettyJson } from '../lib/toolEvents';
import { GenericResult, TOOL_RESULT_RENDERERS } from './toolResultRenderers';

const LONG_VALUE_CHARS = 240;

export function ToolCard({ entry }: { entry: TranscriptToolEntry }) {
  const call = entry.call;
  const result = entry.result;
  const name = call?.name || result?.name || 'unknown tool';
  const args = call?.arguments;
  const subtitle =
    isRecord(args) && typeof args.tool_use_description === 'string'
      ? args.tool_use_description
      : null;

  return (
    <div className="rounded-lg border border-border bg-surface-default p-3">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-semibold font-mono text-sm">{name}</span>
            <Badge variant="outline">{call?.kind ?? 'tool'}</Badge>
            {result === null ? <PendingBadge /> : null}
          </div>
          {subtitle === null ? null : (
            <p className="mt-1 text-muted-foreground text-xs">{subtitle}</p>
          )}
        </div>
        <span className="shrink-0 text-[10px] text-muted-foreground">{entry.event.agent_role}</span>
      </div>

      <CallArguments arguments={args} available={call !== null} />
      <ToolResultSection arguments={args} name={name} result={result} />

      <RawEvents entry={entry} />
    </div>
  );
}

function CallArguments({ arguments: args, available }: { arguments: unknown; available: boolean }) {
  return available ? (
    <ArgumentsTable arguments={args} />
  ) : (
    <p className="mt-3 text-muted-foreground text-xs">Call details unavailable.</p>
  );
}

function ToolResultSection({
  arguments: args,
  name,
  result,
}: {
  arguments: unknown;
  name: string;
  result: DecodedToolResult | null;
}) {
  if (result === null) {
    return (
      <div className="mt-3 border-border border-t pt-3 text-muted-foreground text-xs">
        Waiting for result…
      </div>
    );
  }
  const error = toolError(result.output);
  const Renderer = TOOL_RESULT_RENDERERS[name];
  const ref = spoolRef(result.output);
  return (
    <div className="mt-3 space-y-2 border-border border-t pt-3">
      <div className="flex items-center justify-between gap-2">
        <span className="font-medium text-xs">Result</span>
        <span className="font-mono text-[10px] text-muted-foreground">
          {result.durationMs === null ? 'duration unavailable' : formatDuration(result.durationMs)}
        </span>
      </div>
      {error === null ? (
        Renderer === undefined ? (
          <GenericResult output={result.output} />
        ) : (
          <Renderer arguments={args} output={result.output} />
        )
      ) : (
        <ToolErrorCard error={error} />
      )}
      {ref === null ? null : (
        <div className="rounded-md border border-warning/40 bg-warning/5 p-2 text-warning text-xs">
          Output truncated · full payload spooled at <span className="font-mono">{ref}</span>
        </div>
      )}
    </div>
  );
}

export function StreamingToolCard({ entry }: { entry: TranscriptToolStreamEntry }) {
  return (
    <div className="rounded-lg border border-live/30 bg-surface-default p-3">
      <div className="flex items-center gap-2">
        <span className="font-semibold font-mono text-sm">{entry.name ?? 'tool call'}</span>
        <Badge variant="outline">{entry.kind ?? 'tool'}</Badge>
        <PendingBadge />
      </div>
      {entry.argumentsText === '' ? null : (
        <pre className="mt-3 max-h-32 overflow-auto whitespace-pre-wrap break-words rounded-md bg-surface-subtle p-2 font-mono text-xs">
          {entry.argumentsText}
        </pre>
      )}
      <details className="mt-3 text-xs">
        <summary className="cursor-pointer text-muted-foreground">View raw event</summary>
        <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-surface-subtle p-2 font-mono text-xs">
          {prettyJson(entry.event)}
        </pre>
      </details>
    </div>
  );
}

function ArgumentsTable({ arguments: args }: { arguments: unknown }) {
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const fields = toolArgumentFields(args);
  if (fields.length === 0) {
    return null;
  }
  const toggle = (key: string) => {
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  };
  return (
    <div className="mt-3 overflow-hidden rounded-md border border-border">
      <table className="w-full table-fixed text-xs">
        <tbody className="divide-y divide-border">
          {fields.map(([key, value]) => {
            const rendered = prettyValue(value);
            const isLong = rendered.length > LONG_VALUE_CHARS;
            const isExpanded = expanded.has(key);
            return (
              <tr key={key}>
                <th className="w-1/4 px-2 py-1.5 text-left align-top font-medium text-muted-foreground">
                  {key}
                </th>
                <td className="px-2 py-1.5 align-top">
                  <pre
                    className={`whitespace-pre-wrap break-words font-mono ${
                      isLong && !isExpanded ? 'max-h-16 overflow-hidden' : ''
                    }`}
                  >
                    {rendered}
                  </pre>
                  {isLong ? (
                    <Button
                      className="mt-1 h-6 px-1 text-[10px]"
                      onClick={() => toggle(key)}
                      size="sm"
                      type="button"
                      variant="ghost"
                    >
                      {isExpanded ? 'Collapse value' : 'Expand value'}
                    </Button>
                  ) : null}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function RawEvents({ entry }: { entry: TranscriptToolEntry }) {
  return (
    <details className="mt-3 border-border border-t pt-2 text-xs">
      <summary className="cursor-pointer text-muted-foreground">View raw events</summary>
      <pre className="mt-2 max-h-72 overflow-auto whitespace-pre-wrap break-words rounded-md bg-surface-subtle p-2 font-mono text-xs">
        {prettyJson({
          call: entry.call?.event ?? null,
          result: entry.result?.event ?? null,
          arguments: entry.call?.arguments ?? null,
        })}
      </pre>
    </details>
  );
}

function ToolErrorCard({ error }: { error: ToolError }) {
  return (
    <div className="rounded-md border border-danger/40 bg-danger/5 p-3">
      <div className="flex items-start gap-2">
        <Badge variant="destructive">{error.kind}</Badge>
        <p className="font-medium text-danger text-sm">{error.message}</p>
      </div>
      {error.detail === undefined && error.guidance === undefined ? null : (
        <details className="mt-2 text-xs">
          <summary className="cursor-pointer text-muted-foreground">Details and guidance</summary>
          <div className="mt-2 space-y-2">
            {error.detail === undefined ? null : (
              <pre className="whitespace-pre-wrap break-words font-mono">
                {prettyJson(error.detail)}
              </pre>
            )}
            {error.guidance === undefined ? null : (
              <div className="text-muted-foreground">{prettyValue(error.guidance)}</div>
            )}
          </div>
        </details>
      )}
    </div>
  );
}

type ToolError = {
  kind: string;
  message: string;
  detail?: unknown;
  guidance?: unknown;
};

export function toolError(output: unknown): ToolError | null {
  if (!isRecord(output) || !isRecord(output.error)) {
    return null;
  }
  const value = output.error;
  return {
    kind: typeof value.kind === 'string' ? value.kind : 'error',
    message: typeof value.message === 'string' ? value.message : 'Tool execution failed',
    ...(value.detail === undefined ? {} : { detail: value.detail }),
    ...(value.guidance === undefined ? {} : { guidance: value.guidance }),
  };
}

export function toolArgumentFields(args: unknown): [string, unknown][] {
  if (!isRecord(args)) {
    return args === undefined || args === null ? [] : [['input', args]];
  }
  return Object.entries(args).filter(
    ([key]) => key !== 'tool_use_description' && key !== 'tool_use_metadata'
  );
}

function prettyValue(value: unknown): string {
  return typeof value === 'string' ? value : prettyJson(value);
}

function spoolRef(output: unknown): string | null {
  return isRecord(output) && typeof output.spool_ref === 'string' ? output.spool_ref : null;
}

function formatDuration(durationMs: number): string {
  return durationMs >= 1000 ? `${(durationMs / 1000).toFixed(2)} s` : `${durationMs} ms`;
}

function PendingBadge() {
  return (
    <Badge className="gap-1" variant="secondary">
      <span className="size-2 animate-spin rounded-full border border-current border-t-transparent" />
      pending
    </Badge>
  );
}
