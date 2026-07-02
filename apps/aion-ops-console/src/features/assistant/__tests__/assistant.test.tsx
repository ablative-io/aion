import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { RouteObject } from 'react-router';
import { MemoryRouter } from 'react-router';

import { appRoutes, assistantPath, assistantSessionPath } from '@/app/routes';
import type { AttemptCapabilities } from '@/lib/api';
import { ACTION_IDS, CONSOLE_ACTIONS } from '@/lib/keybindings';
import type { WorkflowSummary } from '@/types';

import { AssistantSessionRows } from '../components/AssistantSessionList';
import { ModeChip, SessionDock } from '../components/AssistantSessionView';
import {
  ASSISTANT_WORKFLOW_TYPE,
  assistantSessionsFilter,
  assistantStartInput,
} from '../lib/contract';
import {
  deriveAssistantMode,
  MODE_PLACEHOLDER,
  selectAssistantAttempt,
  sortSessionsNewestFirst,
} from '../lib/mode';

// --- mode selection: live-attempt vs awaiting-continuation → inject vs signal ---

describe('deriveAssistantMode', () => {
  test('a live attempt means send = intervene inject', () => {
    expect(
      deriveAssistantMode({ isTerminal: false, attemptsReady: true, liveAttemptCount: 1 })
    ).toBe('live');
  });

  test('an open workflow with no live attempt awaits continuation (send = signal)', () => {
    expect(
      deriveAssistantMode({ isTerminal: false, attemptsReady: true, liveAttemptCount: 0 })
    ).toBe('awaiting');
  });

  test('a terminal workflow is ended regardless of the attempt enumeration', () => {
    expect(
      deriveAssistantMode({ isTerminal: true, attemptsReady: false, liveAttemptCount: 0 })
    ).toBe('ended');
    expect(
      deriveAssistantMode({ isTerminal: true, attemptsReady: true, liveAttemptCount: 1 })
    ).toBe('ended');
  });

  test('an unanswered attempt enumeration is connecting — never a guessed verb', () => {
    expect(
      deriveAssistantMode({ isTerminal: false, attemptsReady: false, liveAttemptCount: 0 })
    ).toBe('connecting');
  });
});

test('the chat targets the NEWEST live attempt (rounds are sequential)', () => {
  const attempts: AttemptCapabilities[] = [
    { activityId: 1, attempt: 1, capabilities: { supported: [] } },
    { activityId: 3, attempt: 2, capabilities: { supported: [] } },
  ];
  expect(selectAssistantAttempt(attempts)?.activityId).toBe(3);
  expect(selectAssistantAttempt([])).toBeNull();
});

// --- session-list filtering + ordering ---

test('the session list filters to exactly the assistant workflow type', () => {
  expect(assistantSessionsFilter()).toEqual({
    workflow_type: ASSISTANT_WORKFLOW_TYPE,
    status: null,
    started_after: null,
    started_before: null,
    parent: null,
  });
});

function summary(id: string, startedAt: string): WorkflowSummary {
  return {
    workflow_id: id,
    workflow_type: ASSISTANT_WORKFLOW_TYPE,
    status: 'Running',
    started_at: startedAt,
    ended_at: null,
    parent: null,
  };
}

test('sessions order newest first', () => {
  const sorted = sortSessionsNewestFirst([
    summary('wf-old', '2026-07-01T00:00:00Z'),
    summary('wf-new', '2026-07-03T00:00:00Z'),
    summary('wf-mid', '2026-07-02T00:00:00Z'),
  ]);
  expect(sorted.map((session) => session.workflow_id)).toEqual(['wf-new', 'wf-mid', 'wf-old']);
});

test('session rows deep-link to /assistant/:id with a status chip', () => {
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <AssistantSessionRows sessions={[summary('wf-assist-1', '2026-07-02T00:00:00Z')]} />
    </MemoryRouter>
  );
  expect(markup).toContain('href="/assistant/wf-assist-1"');
  expect(markup).toContain('data-status="Running"');
});

test('the contract start input carries objective + repo_path (blank travels as empty string)', () => {
  expect(assistantStartInput('fix the flaky test', '/repo')).toEqual({
    objective: 'fix the flaky test',
    repo_path: '/repo',
  });
  expect(assistantStartInput('fix the flaky test', '')).toEqual({
    objective: 'fix the flaky test',
    repo_path: '',
  });
});

// --- the docked chat makes its send verb obvious per mode ---

describe('SessionDock', () => {
  const noop = () => {};

  test('live mode: interrupt placeholder + live dot + streaming send', () => {
    const markup = renderToStaticMarkup(
      <SessionDock
        chatExpanded={false}
        mode="live"
        onEscalate={noop}
        onExpandedChange={noop}
        onSubmit={noop}
        workflowId="wf-1"
      />
    );
    expect(markup).toContain(MODE_PLACEHOLDER.live);
    expect(markup).toContain('data-status="live"');
    expect(markup).toContain('data-streaming="true"');
  });

  test('awaiting mode: continue placeholder, not streaming', () => {
    const markup = renderToStaticMarkup(
      <SessionDock
        chatExpanded={false}
        mode="awaiting"
        onEscalate={noop}
        onExpandedChange={noop}
        onSubmit={noop}
        workflowId="wf-1"
      />
    );
    expect(markup).toContain(MODE_PLACEHOLDER.awaiting);
    expect(markup).not.toContain('data-streaming');
  });

  test('ended mode offers no input at all', () => {
    const markup = renderToStaticMarkup(
      <SessionDock
        chatExpanded={false}
        mode="ended"
        onEscalate={noop}
        onExpandedChange={noop}
        onSubmit={noop}
        workflowId="wf-1"
      />
    );
    expect(markup).not.toContain('data-slot="chat-pill"');
    expect(markup).toContain('Session ended');
  });

  test('mode chip labels the session state', () => {
    expect(renderToStaticMarkup(<ModeChip mode="live" />)).toContain('Agent working');
    expect(renderToStaticMarkup(<ModeChip mode="awaiting" />)).toContain('Awaiting continuation');
  });
});

// --- route + keybinding registration ---

function collectPaths(routes: RouteObject[]): string[] {
  return routes.flatMap((route) => [
    ...(route.path === undefined ? [] : [route.path]),
    ...collectPaths(route.children ?? []),
  ]);
}

test('the assistant routes are registered under the shell', () => {
  const paths = collectPaths(appRoutes);
  expect(paths).toContain(assistantPath);
  expect(paths).toContain(assistantSessionPath);
});

test('assistant keybindings live in the central registry inventory (mod = ⌘/Super)', () => {
  const goAssistant = CONSOLE_ACTIONS.find((action) => action.id === ACTION_IDS.goAssistant);
  expect(goAssistant?.defaultBinding).toBe('g a');
  expect(goAssistant?.scope).toBe('global');

  const focusChat = CONSOLE_ACTIONS.find((action) => action.id === ACTION_IDS.assistantFocusChat);
  expect(focusChat?.defaultBinding).toBe('mod+i');
  expect(focusChat?.scope).toBe('view');
  expect(focusChat?.allowInInputs).toBe(true);
});
