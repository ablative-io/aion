/**
 * Parser for the harness's raw `reasoning_item` passthrough (NOI-7).
 *
 * A reasoning item rides an `ActivityEventKind::Raw` verbatim from the harness:
 * `{ type: "reasoning_item", item: { id, summary: [{ type: "summary_text", text }],
 * encrypted_content, ... } }`. The console renders ONLY the human-readable summary
 * text(s) — the encrypted blob is provider-opaque and must never be dumped — and
 * uses the item id to finalize the matching thinking-delta stream.
 */

export type ReasoningItem = {
  /** The harness item id (matches the thinking-delta `message_id`), when present. */
  id: string | null;
  /** The human-readable summary texts, in order (may be empty). */
  summaries: string[];
};

/** Narrow an unknown JSON value to a plain object record. */
function asRecord(value: unknown): Record<string, unknown> | null {
  if (typeof value === 'object' && value !== null && !Array.isArray(value)) {
    return value as Record<string, unknown>;
  }
  return null;
}

/**
 * Parse a raw event payload as a reasoning item, or `null` when it is not one.
 * Recognition keys on the payload's own `type: "reasoning_item"` label (the raw
 * `source` is just the transport method and is not shape-bearing).
 */
export function parseReasoningItem(value: unknown): ReasoningItem | null {
  const record = asRecord(value);
  if (record === null || record.type !== 'reasoning_item') {
    return null;
  }
  const item = asRecord(record.item);
  if (item === null) {
    return null;
  }
  const id = typeof item.id === 'string' ? item.id : null;
  const summaries: string[] = [];
  if (Array.isArray(item.summary)) {
    for (const part of item.summary) {
      const summary = asRecord(part);
      if (summary !== null && typeof summary.text === 'string' && summary.text.length > 0) {
        summaries.push(summary.text);
      }
    }
  }
  return { id, summaries };
}
