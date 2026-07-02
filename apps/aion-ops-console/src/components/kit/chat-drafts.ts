// Draft persistence for the chat-input morph: collapsing the input must never
// eat the operator's half-written message. Backed by the shared draft-store
// mechanism (zustand + sessionStorage, src/lib/state) so drafts survive
// unmount/remount, view switches, and reloads within the tab; keyed so every
// conversation keeps its own.

import { createDraftStore } from '@/lib/state';

type ChatDrafts = { byKey: Record<string, string> };

const chatDraftStore = createDraftStore<ChatDrafts>({
  name: 'chat',
  empty: { byKey: {} },
  // Drafts are a dynamic record, so the default shallow shape check cannot
  // vouch for the values: keep only string entries from a hydrated blob.
  sanitize: (persisted) => {
    const byKey: Record<string, string> = {};
    if (typeof persisted === 'object' && persisted !== null) {
      const candidate = (persisted as { byKey?: unknown }).byKey;
      if (typeof candidate === 'object' && candidate !== null) {
        for (const [key, value] of Object.entries(candidate)) {
          if (typeof value === 'string') {
            byKey[key] = value;
          }
        }
      }
    }
    return { byKey };
  },
});

export function readChatDraft(draftKey: string): string {
  return chatDraftStore.getState().draft.byKey[draftKey] ?? '';
}

export function writeChatDraft(draftKey: string, value: string): void {
  const byKey = { ...chatDraftStore.getState().draft.byKey };
  if (value === '') {
    delete byKey[draftKey];
  } else {
    byKey[draftKey] = value;
  }
  chatDraftStore.getState().setDraft({ byKey });
}

export function clearChatDraft(draftKey: string): void {
  writeChatDraft(draftKey, '');
}
