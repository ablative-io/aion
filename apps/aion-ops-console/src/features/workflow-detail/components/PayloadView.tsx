import { ChevronRight } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui';
import { cn } from '@/lib/utils';

import { decodePayload, payloadSummary } from '../lib/timeline';

type PayloadViewProps = {
  payload: unknown;
  label?: string;
};

function PayloadView({ payload, label = 'Payload' }: PayloadViewProps) {
  const [open, setOpen] = useState(false);
  const formattedPayload = stringifyPayload(payload);

  return (
    <div className="mt-3 rounded-lg border border-border bg-surface-base">
      <Button
        aria-expanded={open}
        className="h-auto w-full justify-start rounded-lg px-3 py-2 text-left"
        onClick={() => setOpen((current) => !current)}
        type="button"
        variant="ghost"
      >
        <ChevronRight className={cn('size-4 transition-transform', open && 'rotate-90')} />
        <span className="font-medium text-secondary-foreground text-xs uppercase tracking-wide">
          {label}
        </span>
        <span className="truncate text-muted-foreground text-sm">{payloadSummary(payload)}</span>
      </Button>
      {open ? (
        <pre className="max-h-72 overflow-auto border-border border-t p-3 text-secondary-foreground text-xs leading-relaxed">
          {formattedPayload}
        </pre>
      ) : null}
    </div>
  );
}

function stringifyPayload(payload: unknown): string {
  const decodedPayload = decodePayload(payload);

  try {
    return JSON.stringify(decodedPayload, null, 2) ?? String(decodedPayload);
  } catch {
    return String(decodedPayload);
  }
}

export { PayloadView, stringifyPayload };
