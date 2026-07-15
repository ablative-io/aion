import type { ActivityEvent } from '@/types';

import { parseReasoningItem } from './reasoning';

/**
 * Transcript entry model + pure fold logic (NOI-7).
 *
 * Dedup + order rules (mirroring the server splice contract):
 *
 * - A PERSISTED event carries a numeric `store_seq`. Two persisted events with the
 *   same `store_seq` are the SAME event (a reconnect race re-delivered one); the
 *   later arrival is dropped. Persisted events are kept ordered by `store_seq`.
 * - An EPHEMERAL token delta carries `store_seq: null`. It is never persisted and
 *   never replayed; it is retained after the last persisted event.
 *
 * Coalescing rules (one growing entry, never one row per token/chunk):
 *
 * - Ephemeral `Delta`s for one `(agent_id, message_id)` accumulate into a single
 *   {@link TranscriptStreamEntry} whose text grows in place. The stream entry is
 *   REMOVED when its persisted finalizer arrives — the agent's complete `Message`,
 *   or the `reasoning_item` raw carrying the same item id — so the final event
 *   never renders alongside its own live preview. (This matches a reload: the
 *   ephemeral preview is never persisted; the finalizer is the durable truth.)
 * - Consecutive identical `Progress` notes from one agent (e.g. the harness's
 *   payload-free `tool_call_delta` chunk markers) coalesce into a single counted
 *   {@link TranscriptNoteRunEntry} instead of one content-free row per chunk.
 */

/** A single rendered transcript event (the common case). */
export type TranscriptEventEntry = {
  type: 'event';
  /** Stable key: the store_seq for persisted events, or a monotonic id otherwise. */
  key: string;
  event: ActivityEvent;
};

/**
 * An in-progress streamed message: every ephemeral `Delta` for one
 * `(agent_id, message_id)` coalesces into this single entry whose {@link text}
 * grows in place, streaming onto the same row.
 */
export type TranscriptStreamEntry = {
  type: 'stream';
  /** Stable key derived from the stream identity (agent + message id). */
  key: string;
  /** The harness message/item id the coalesced deltas belong to. */
  messageId: string;
  /** The text accumulated so far. */
  text: string;
  /** The latest constituent delta (attribution + ephemeral flag for rendering). */
  event: ActivityEvent;
};

/**
 * A run of consecutive identical progress notes coalesced into one counted entry.
 * The absorbed `store_seq`s are kept so a reconnect replay of any member is still
 * recognised as a duplicate.
 */
export type TranscriptNoteRunEntry = {
  type: 'notes';
  key: string;
  /** The note label shared by every event in the run. */
  text: string;
  /** How many identical notes the run has absorbed. */
  count: number;
  /** The persisted `store_seq`s absorbed into the run (replay dedupe). */
  seqs: number[];
  /** The latest constituent event. */
  event: ActivityEvent;
};

/** A rendered transcript entry: one event, one live stream, or one note run. */
export type TranscriptEntry = TranscriptEventEntry | TranscriptStreamEntry | TranscriptNoteRunEntry;

/**
 * Fold one incoming event into the ordered, deduplicated, coalesced entry list.
 *
 * - An ephemeral `Delta` accumulates into its `(agent_id, message_id)` stream
 *   entry (created at the tail on first fragment), growing its text in place.
 * - A persisted event (numeric `store_seq`) is deduplicated on that sequence,
 *   finalizes any live stream it completes, and is inserted in `store_seq` order —
 *   except an identical consecutive `Progress` note, which is absorbed into the
 *   tail note run.
 * - `deltaId` is a caller-supplied monotonic id keying entries that have no
 *   sequence of their own, so React keys stay stable and unique.
 *
 * Returns the SAME array reference when a persisted duplicate is dropped, so React
 * can skip the re-render.
 */
export function foldTranscriptEvent(
  entries: TranscriptEntry[],
  event: ActivityEvent,
  deltaId: number
): TranscriptEntry[] {
  if (event.kind.kind === 'Delta') {
    return foldDelta(entries, event, event.kind.message_id, event.kind.text_fragment);
  }

  const storeSeq = event.store_seq;
  if (typeof storeSeq === 'number' && hasStoreSeq(entries, storeSeq)) {
    // A reconnect race re-delivered this persisted event: drop the duplicate.
    return entries;
  }

  if (event.kind.kind === 'Progress' && event.kind.detail.detail === 'Note') {
    return foldNote(entries, event, event.kind.detail.text, deltaId);
  }

  const next = withFinalizedStreams(entries, event);
  if (typeof storeSeq !== 'number') {
    // No sequence yet: unique by arrival, appended at the tail.
    return [...next, { type: 'event', key: `live:${deltaId}`, event }];
  }

  // Insert in store_seq order among the persisted entries. `Array.prototype.sort`
  // is stable, so entries without a sequence keep their arrival order and any
  // persisted-vs-ephemeral pair is left in arrival order (comparator returns 0).
  const inserted: TranscriptEntry[] = [...next, { type: 'event', key: `seq:${storeSeq}`, event }];
  inserted.sort(comparePersistedThenArrival);
  return inserted;
}

/** Accumulate one ephemeral delta fragment into its stream entry (creating it). */
function foldDelta(
  entries: TranscriptEntry[],
  event: ActivityEvent,
  messageId: string,
  fragment: string
): TranscriptEntry[] {
  const key = `stream:${event.agent_id}:${messageId}`;
  const index = entries.findLastIndex((entry) => entry.type === 'stream' && entry.key === key);
  const current = index === -1 ? undefined : entries[index];
  if (current === undefined || current.type !== 'stream') {
    return [...entries, { type: 'stream', key, messageId, text: fragment, event }];
  }
  const next = [...entries];
  next[index] = { ...current, text: current.text + fragment, event };
  return next;
}

/** Absorb one progress note into the tail run when identical, else start a run. */
function foldNote(
  entries: TranscriptEntry[],
  event: ActivityEvent,
  text: string,
  deltaId: number
): TranscriptEntry[] {
  const storeSeq = event.store_seq;
  const last = entries.at(-1);
  if (
    last !== undefined &&
    last.type === 'notes' &&
    last.text === text &&
    last.event.agent_id === event.agent_id
  ) {
    const next = [...entries];
    next[next.length - 1] = {
      ...last,
      count: last.count + 1,
      seqs: typeof storeSeq === 'number' ? [...last.seqs, storeSeq] : last.seqs,
      event,
    };
    return next;
  }

  const entry: TranscriptNoteRunEntry = {
    type: 'notes',
    key: typeof storeSeq === 'number' ? `seq:${storeSeq}` : `live:${deltaId}`,
    text,
    count: 1,
    seqs: typeof storeSeq === 'number' ? [storeSeq] : [],
    event,
  };
  const next: TranscriptEntry[] = [...entries, entry];
  next.sort(comparePersistedThenArrival);
  return next;
}

/**
 * Remove any live stream entry this persisted event finalizes, so the complete
 * event never renders alongside its own token-delta preview:
 *
 * - a complete `Message` supersedes the producing agent's open streams (the
 *   message kind carries no item id, so attribution is the join key), and
 * - a `reasoning_item` raw supersedes the thinking stream with the same item id.
 */
function withFinalizedStreams(entries: TranscriptEntry[], event: ActivityEvent): TranscriptEntry[] {
  if (event.kind.kind === 'Message') {
    return withoutStreams(entries, (stream) => stream.event.agent_id === event.agent_id);
  }
  if (event.kind.kind === 'Raw') {
    const reasoning = parseReasoningItem(event.kind.value);
    if (reasoning !== null && reasoning.id !== null) {
      return withoutStreams(entries, (stream) => stream.messageId === reasoning.id);
    }
  }
  return entries;
}

/** Drop matching stream entries; returns the SAME array when none match. */
function withoutStreams(
  entries: TranscriptEntry[],
  matches: (stream: TranscriptStreamEntry) => boolean
): TranscriptEntry[] {
  if (!entries.some((entry) => entry.type === 'stream' && matches(entry))) {
    return entries;
  }
  return entries.filter((entry) => !(entry.type === 'stream' && matches(entry)));
}

/** Whether `storeSeq` is already represented (as an entry or inside a note run). */
function hasStoreSeq(entries: TranscriptEntry[], storeSeq: number): boolean {
  return entries.some((entry) =>
    entry.type === 'notes'
      ? entry.seqs.includes(storeSeq)
      : entry.type === 'event' && entry.event.store_seq === storeSeq
  );
}

/** The sequence an entry sorts by: its own, or a note run's first absorbed one. */
function entrySeq(entry: TranscriptEntry): number | null {
  if (entry.type === 'notes') {
    return entry.seqs[0] ?? null;
  }
  if (entry.type === 'stream') {
    return null;
  }
  return entry.event.store_seq;
}

/** Order persisted entries by store_seq; any pair lacking one keeps arrival order. */
function comparePersistedThenArrival(left: TranscriptEntry, right: TranscriptEntry): number {
  const leftSeq = entrySeq(left);
  const rightSeq = entrySeq(right);
  if (typeof leftSeq === 'number' && typeof rightSeq === 'number') {
    return leftSeq - rightSeq;
  }
  return 0;
}

/**
 * Fold a REST-retained transcript (`store_seq` order) into entries + the resume
 * cursor. `lastSeq` is the highest persisted `store_seq` seen — the WS attaches
 * with `after_seq = lastSeq` so it serves only the live tail. An empty retained
 * transcript reports no cursor (pre-retention run ⇒ full WS replay, honest).
 */
export function backfillEntries(events: readonly ActivityEvent[]): {
  entries: TranscriptEntry[];
  lastSeq: number | undefined;
} {
  let entries: TranscriptEntry[] = [];
  let lastSeq: number | undefined;
  let deltaId = 0;
  for (const event of events) {
    entries = foldTranscriptEvent(entries, event, deltaId);
    deltaId += 1;
    if (typeof event.store_seq === 'number') {
      lastSeq = lastSeq === undefined ? event.store_seq : Math.max(lastSeq, event.store_seq);
    }
  }
  return { entries, lastSeq };
}
