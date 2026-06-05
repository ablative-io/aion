import { describe, expect, test } from 'bun:test';
import { Search } from 'lucide-react';
import type { ReactElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

import { EmptyState } from '@/components/EmptyState';
import { ErrorState } from '@/components/ErrorState';
import { EventIcon, type EventIconKind } from '@/components/EventIcon';
import { LoadingSkeleton } from '@/components/LoadingSkeleton';
import { StatusBadge } from '@/components/StatusBadge';
import type { WorkflowStatus } from '@/types';

const workflowStatuses = [
  'Running',
  'Completed',
  'Failed',
  'Cancelled',
  'TimedOut',
] satisfies WorkflowStatus[];

const eventIconKinds = ['lifecycle', 'activity', 'timer', 'signal', 'child'] satisfies EventIconKind[];

describe('StatusBadge', () => {
  test('renders a distinct badge for every generated workflow status', () => {
    const markups = workflowStatuses.map((status) =>
      renderToStaticMarkup(<StatusBadge status={status} />)
    );

    expect(new Set(markups).size).toBe(workflowStatuses.length);
    for (const status of workflowStatuses) {
      expect(renderToStaticMarkup(<StatusBadge status={status} />)).toContain(
        `data-status="${status}"`
      );
    }
  });
});

describe('EventIcon', () => {
  test('renders a distinct icon and colour affordance for every event category kind', () => {
    const markups = eventIconKinds.map((kind) =>
      renderToStaticMarkup(<EventIcon kind={kind} />)
    );

    expect(new Set(markups).size).toBe(eventIconKinds.length);
    for (const kind of eventIconKinds) {
      expect(renderToStaticMarkup(<EventIcon kind={kind} />)).toContain(
        `data-event-kind="${kind}"`
      );
    }
  });
});

describe('shared async state panels', () => {
  test('EmptyState renders a configurable message and optional icon', () => {
    const markup = renderToStaticMarkup(
      <EmptyState
        icon={<Search aria-hidden="true" />}
        message="Try a wider filter."
        title="No matches"
      />
    );

    expect(markup).toContain('No matches');
    expect(markup).toContain('Try a wider filter.');
    expect(markup).toContain('lucide-search');
  });

  test('ErrorState renders a cause and exposes a retry button callback', () => {
    let retryCount = 0;
    const onRetry = () => {
      retryCount += 1;
    };
    const element = (
      <ErrorState error="server unavailable" onRetry={onRetry} title="Load failed" />
    );
    const markup = renderToStaticMarkup(element);

    expect(markup).toContain('Load failed');
    expect(markup).toContain('server unavailable');
    expect(markup).toContain('Retry');
    const rendered = ErrorState({
      error: 'server unavailable',
      onRetry,
      title: 'Load failed',
    }) as ReactElement<{
      children: ReactElement<{ onClick: () => void }>[];
    }>;
    rendered.props.children[1]?.props.onClick();
    expect(retryCount).toBe(1);
  });

  test('LoadingSkeleton renders distinct layout-reserving list and timeline variants', () => {
    const list = renderToStaticMarkup(
      <LoadingSkeleton label="Loading workflow summaries" rows={2} variant="list" />
    );
    const timeline = renderToStaticMarkup(
      <LoadingSkeleton label="Loading timeline" rows={2} variant="timeline" />
    );

    expect(list).toContain('Loading workflow summaries');
    expect(timeline).toContain('Loading timeline');
    expect(list).not.toEqual(timeline);
    expect(list).toContain('data-slot="skeleton"');
    expect(timeline).toContain('data-slot="skeleton"');
  });
});
