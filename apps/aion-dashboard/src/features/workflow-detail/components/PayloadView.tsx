import { ChevronRight } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui';
import { cn } from '@/lib/utils';

import { payloadSummary } from '../lib/timeline';

type PayloadViewProps = {
  payload: unknown;
  label?: string;
};

function PayloadView({ payload, label = 'Payload' }: PayloadViewProps) {
  const [open, setOpen] = useState(false);
  const formattedPayload = stringifyPayload(payload);

  return (
    <div className="mt-3 rounded-lg border border-[var(--border-default)] bg-[var(--surface-base)]">
      <Button
        aria-expanded={open}
        className="h-auto w-full justify-start rounded-lg px-3 py-2 text-left"
        onClick={() => setOpen((current) => !current)}
        type="button"
        variant="ghost"
      >
        <ChevronRight className={cn('size-4 transition-transform', open && 'rotate-90')} />
        <span className="font-medium text-[var(--text-secondary)] text-xs uppercase tracking-wide">
          {label}
        </span>
        <span className="truncate text-[var(--text-muted)] text-sm">{payloadSummary(payload)}</span>
      </Button>
      {open ? (
        <pre className="max-h-72 overflow-auto border-[var(--border-default)] border-t p-3 text-[var(--text-secondary)] text-xs leading-relaxed">
          {formattedPayload}
        </pre>
      ) : null}
    </div>
  );
}

function stringifyPayload(payload: unknown): string {
  try {
    return JSON.stringify(payload, null, 2) ?? String(payload);
  } catch {
    return String(payload);
  }
}

export { PayloadView, stringifyPayload };
