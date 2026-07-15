import { expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import type { AttemptNavigatorRow } from '../lib/attemptNavigator';
import { AttemptRowList } from '../swimlane/AttemptNavigator';

const rows: AttemptNavigatorRow[] = [
  {
    key: '3:1',
    activityId: '3',
    activityType: 'agent',
    attempt: 1,
    isLegacy: false,
    status: 'completed',
  },
  {
    key: '3:2',
    activityId: '3',
    activityType: 'agent',
    attempt: 2,
    isLegacy: false,
    status: 'started',
  },
];

test('attempt rows badge retained transcripts from the enumeration', () => {
  const markup = renderToStaticMarkup(
    <AttemptRowList
      onSelect={() => {}}
      retainedKeys={new Set(['3:1'])}
      rows={rows}
      selectedKey={null}
    />
  );

  // The badge appears exactly for the enumerated row: rows stay
  // timeline-derived; the retained enumeration is a badge source only.
  expect(markup).toContain('data-testid="retained:3:1"');
  expect(markup).toContain('retained');
  expect(markup).not.toContain('retained:3:2');
});
