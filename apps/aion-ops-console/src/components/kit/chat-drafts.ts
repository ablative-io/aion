// Draft persistence for the chat-input morph: collapsing the input must never
// eat the operator's half-written message. Module-level (not component state)
// so the draft survives unmount/remount across collapse cycles and view
// switches within the session; keyed so every conversation keeps its own.

const chatDrafts = new Map<string, string>();

export function readChatDraft(draftKey: string): string {
  return chatDrafts.get(draftKey) ?? '';
}

export function writeChatDraft(draftKey: string, value: string): void {
  if (value === '') {
    chatDrafts.delete(draftKey);
    return;
  }
  chatDrafts.set(draftKey, value);
}

export function clearChatDraft(draftKey: string): void {
  chatDrafts.delete(draftKey);
}
