import type { ReactNode } from 'react';

import { Badge } from '@/components/ui';

import { isRecord, prettyJson } from '../lib/toolEvents';

export type ToolResultRendererProps = {
  output: unknown;
  arguments: unknown;
};

export type ToolResultRenderer = (props: ToolResultRendererProps) => ReactNode;

const MONO_BLOCK =
  'max-h-72 overflow-auto rounded-md border border-border bg-surface-subtle p-2 whitespace-pre-wrap break-words font-mono text-xs';

function valueString(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

function numberValue(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function SummaryLine({ children }: { children: ReactNode }) {
  return <div className="text-muted-foreground text-xs">{children}</div>;
}

function MonoBlock({ children, label }: { children: string; label?: string }) {
  if (children === '') {
    return null;
  }
  return (
    <div className="space-y-1">
      {label === undefined ? null : (
        <div className="font-medium text-[10px] text-muted-foreground uppercase tracking-wide">
          {label}
        </div>
      )}
      <pre className={MONO_BLOCK}>{children}</pre>
    </div>
  );
}

function ReadResult({ output }: ToolResultRendererProps) {
  if (!isRecord(output)) {
    return <GenericResult output={output} />;
  }
  const kind = valueString(output.kind);
  if (kind === 'text') {
    return <ReadTextResult output={output} />;
  }
  if (kind === 'image' || kind === 'binary') {
    return <ReadFileResult kind={kind} output={output} />;
  }
  return <GenericResult output={output} />;
}

function ReadTextResult({ output }: { output: Record<string, unknown> }) {
  const path = valueString(output.path);
  const totalLines = numberValue(output.total_lines);
  const returnedLines = numberValue(output.returned_lines);
  const warnings = Array.isArray(output.warnings) ? output.warnings : [];
  return (
    <div className="space-y-2">
      <SummaryLine>
        {[path, totalLines === null ? null : `${totalLines} total lines`]
          .filter((part) => part !== null)
          .join(' · ')}
      </SummaryLine>
      <MonoBlock>{valueString(output.content) ?? prettyJson(output.content)}</MonoBlock>
      {output.truncated === true ? (
        <div className="text-warning text-xs">
          Read window truncated
          {returnedLines === null ? '' : ` after ${returnedLines} lines`}
        </div>
      ) : null}
      {warnings.length === 0 ? null : (
        <ul className="list-disc space-y-1 pl-5 text-warning text-xs">
          {keyedValues(warnings).map(({ key, value }) => (
            <li key={key}>{warningText(value)}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ReadFileResult({ kind, output }: { kind: string; output: Record<string, unknown> }) {
  const size = numberValue(output.size_bytes);
  return (
    <div className="flex flex-wrap items-center gap-2 text-xs">
      <Badge variant="outline">{kind}</Badge>
      <span className="font-mono">{valueString(output.path) ?? 'unknown path'}</span>
      {size === null ? null : <span className="text-muted-foreground">{size} bytes</span>}
      {typeof output.message === 'string' ? (
        <span className="text-muted-foreground">{output.message}</span>
      ) : null}
    </div>
  );
}

function BashResult({ output, arguments: args }: ToolResultRendererProps) {
  if (!isRecord(output)) {
    return <GenericResult output={output} />;
  }
  const command = isRecord(args) ? valueString(args.command) : null;
  const exitCode = numberValue(output.exit_code);
  return (
    <div className="space-y-2">
      {command === null ? null : <MonoBlock label="command">{`$ ${command}`}</MonoBlock>}
      <MonoBlock label="stdout">{valueString(output.stdout) ?? ''}</MonoBlock>
      <MonoBlock label="stderr">{valueString(output.stderr) ?? ''}</MonoBlock>
      <div className="flex flex-wrap items-center gap-2 text-xs">
        {exitCode === null ? null : (
          <Badge variant={exitCode === 0 ? 'secondary' : 'destructive'}>exit {exitCode}</Badge>
        )}
        {output.timed_out === true ? <Badge variant="destructive">timed out</Badge> : null}
        {output.background === true || output.migrated === true ? (
          <Badge variant="outline">
            background · {valueString(output.process_id) ?? 'running'}
          </Badge>
        ) : null}
        {output.output_redirected === true ? (
          <span className="text-warning">
            Output redirected to {valueString(output.output_path) ?? 'disk'}
          </span>
        ) : null}
      </div>
    </div>
  );
}

function SearchResult({ output }: ToolResultRendererProps) {
  if (!isRecord(output)) {
    return <GenericResult output={output} />;
  }
  const items = Array.isArray(output.matches)
    ? output.matches
    : Array.isArray(output.paths)
      ? output.paths
      : null;
  if (items === null) {
    return <GenericResult output={output} />;
  }
  return (
    <div className="space-y-2">
      <SummaryLine>{items.length} matches</SummaryLine>
      <ul className="max-h-80 divide-y divide-border overflow-auto rounded-md border border-border">
        {keyedValues(items).map(({ key, value }) => (
          <li className="p-2 font-mono text-xs" key={key}>
            {searchMatch(value)}
          </li>
        ))}
      </ul>
    </div>
  );
}

function WriteResult({ output }: ToolResultRendererProps) {
  if (!isRecord(output)) {
    return <GenericResult output={output} />;
  }
  const path = valueString(output.path);
  const bytes = numberValue(output.bytes_written);
  if (path === null && bytes === null) {
    return <GenericResult output={output} />;
  }
  return (
    <div className="flex flex-wrap items-center gap-2 text-xs">
      <span className="font-mono">{path ?? 'unknown path'}</span>
      {bytes === null ? null : <Badge variant="secondary">{bytes} bytes written</Badge>}
      {numberValue(output.line_count) === null ? null : (
        <span className="text-muted-foreground">{String(output.line_count)} lines</span>
      )}
    </div>
  );
}

function EditResult({ output }: ToolResultRendererProps) {
  if (!isRecord(output)) {
    return <GenericResult output={output} />;
  }
  const diff = diffText(output);
  if (diff !== null) {
    return <DiffBlock diff={diff} />;
  }
  const path = valueString(output.path);
  const kind = valueString(output.kind);
  if (path === null && kind === null) {
    return <GenericResult output={output} />;
  }
  return (
    <div className="space-y-2 text-xs">
      <div className="flex flex-wrap items-center gap-2">
        {path === null ? null : <span className="font-mono">{path}</span>}
        {kind === null ? null : <Badge variant="secondary">{kind.replaceAll('_', ' ')}</Badge>}
      </div>
      {isRecord(output.blast_radius) ? (
        <SummaryLine>{compactFields(output.blast_radius)}</SummaryLine>
      ) : null}
    </div>
  );
}

function PatchResult({ output }: ToolResultRendererProps) {
  if (!isRecord(output)) {
    return <GenericResult output={output} />;
  }
  const diff = diffText(output);
  if (diff !== null) {
    return <DiffBlock diff={diff} />;
  }
  if (!Array.isArray(output.files_modified) && !Array.isArray(output.per_file)) {
    return <GenericResult output={output} />;
  }
  const files: unknown[] = Array.isArray(output.per_file)
    ? output.per_file
    : Array.isArray(output.files_modified)
      ? output.files_modified
      : [];
  return (
    <div className="space-y-2">
      <SummaryLine>
        {numberValue(output.hunks_applied) ?? 0} hunks · +{numberValue(output.lines_added) ?? 0} / −
        {numberValue(output.lines_removed) ?? 0}
      </SummaryLine>
      <ul className="space-y-1 font-mono text-xs">
        {keyedValues(files).map(({ key, value }) => (
          <li key={key}>{patchFile(value)}</li>
        ))}
      </ul>
    </div>
  );
}

export function GenericResult({ output }: Pick<ToolResultRendererProps, 'output'>) {
  return <pre className={MONO_BLOCK}>{prettyJson(output)}</pre>;
}

function DiffBlock({ diff }: { diff: string }) {
  return (
    <pre className={MONO_BLOCK}>
      {keyedValues(diff.split('\n')).map(({ key, value }) => (
        <span
          className={
            value.startsWith('+')
              ? 'block text-success'
              : value.startsWith('-')
                ? 'block text-danger'
                : 'block'
          }
          key={key}
        >
          {value || ' '}
        </span>
      ))}
    </pre>
  );
}

function diffText(output: Record<string, unknown>): string | null {
  for (const key of ['patch', 'diff']) {
    if (typeof output[key] === 'string') {
      return output[key];
    }
  }
  if (typeof output.before === 'string' && typeof output.after === 'string') {
    return `--- before\n${output.before}\n+++ after\n${output.after}`;
  }
  return null;
}

function warningText(warning: unknown): string {
  if (typeof warning === 'string') {
    return warning;
  }
  if (isRecord(warning) && typeof warning.message === 'string') {
    return warning.message;
  }
  return prettyJson(warning);
}

function searchMatch(item: unknown): string {
  if (typeof item === 'string') {
    return item;
  }
  if (!isRecord(item)) {
    return prettyJson(item);
  }
  const path = valueString(item.path) ?? '';
  const line = numberValue(item.line);
  const location = line === null ? path : `${path}:${line}`;
  const content = valueString(item.content);
  return content === null ? location || prettyJson(item) : `${location}  ${content}`;
}

function patchFile(file: unknown): string {
  if (typeof file === 'string') {
    return file;
  }
  if (!isRecord(file)) {
    return prettyJson(file);
  }
  const path = valueString(file.path) ?? 'unknown path';
  const status = valueString(file.status);
  return status === null ? path : `${path} · ${status}`;
}

function compactFields(value: Record<string, unknown>): string {
  return Object.entries(value)
    .filter(([, field]) => typeof field === 'string' || typeof field === 'number')
    .map(([key, field]) => `${key.replaceAll('_', ' ')}: ${String(field)}`)
    .join(' · ');
}

/** Plain registry: tool name is the only extension point for future renderers. */
export const TOOL_RESULT_RENDERERS: Record<string, ToolResultRenderer> = {
  apply_patch: PatchResult,
  bash: BashResult,
  edit: EditResult,
  read: ReadResult,
  search: SearchResult,
  write: WriteResult,
};

/** Content-derived keys with duplicate ordinals, stable for an immutable result payload. */
function keyedValues<T>(values: T[]): { key: string; value: T }[] {
  const occurrences = new Map<string, number>();
  return values.map((value) => {
    const rendered = prettyJson(value);
    const occurrence = occurrences.get(rendered) ?? 0;
    occurrences.set(rendered, occurrence + 1);
    return {
      key: `${rendered}:${occurrence}`,
      value,
    };
  });
}
