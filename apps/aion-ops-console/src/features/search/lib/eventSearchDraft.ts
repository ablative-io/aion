import { createDraftStore } from '@/lib/state';

import { EMPTY_EVENT_SEARCH_FORM, type EventSearchFormState } from '../hooks/useEventSearch';

/**
 * The search fields survive navigating away and back; the Clear button (not a
 * submit) resets them — a submitted query should stay visible with its results.
 */
export const eventSearchDraftStore = createDraftStore<EventSearchFormState>({
  name: 'event-search',
  empty: EMPTY_EVENT_SEARCH_FORM,
});
