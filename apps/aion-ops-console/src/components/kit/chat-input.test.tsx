import { afterEach, describe, expect, jest, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';

import { clearChatDraft, readChatDraft, writeChatDraft } from './chat-drafts';
import type { EscalationLevel } from './chat-escalation';
import { createEscalationMachine, decay, escalate } from './chat-escalation';
import { ChatInputMorph } from './chat-input';

describe('chat draft persistence', () => {
  afterEach(() => {
    clearChatDraft('run-1');
  });

  test('drafts survive by key and clear when emptied', () => {
    expect(readChatDraft('run-1')).toBe('');
    writeChatDraft('run-1', 'hold on, checking the shard map');
    expect(readChatDraft('run-1')).toBe('hold on, checking the shard map');
    writeChatDraft('run-1', '');
    expect(readChatDraft('run-1')).toBe('');
  });

  test('a persisted draft is restored into a remounted expanded input', () => {
    writeChatDraft('run-1', 'draft under test');
    const markup = renderToStaticMarkup(<ChatInputMorph defaultExpanded draftKey="run-1" />);
    expect(markup).toContain('draft under test');
  });

  test('a persisted draft previews on the collapsed pill instead of the placeholder', () => {
    writeChatDraft('run-1', 'draft under test');
    const markup = renderToStaticMarkup(
      <ChatInputMorph draftKey="run-1" placeholder="Message the agent…" />
    );
    expect(markup).toContain('draft under test');
    expect(markup).toContain('data-expanded="false"');
  });
});

describe('chat input render states', () => {
  test('collapsed pill shows status dot, placeholder and capability badges', () => {
    const markup = renderToStaticMarkup(
      <ChatInputMorph capabilities={['interrupt', 'queue']} draftKey="k" status="live" />
    );
    expect(markup).toContain('data-slot="chat-pill"');
    expect(markup).toContain('data-status="live"');
    expect(markup).toContain('interrupt');
    expect(markup).toContain('queue');
  });

  test('expanded form renders textarea, priority toggle and the send button', () => {
    const markup = renderToStaticMarkup(<ChatInputMorph defaultExpanded draftKey="k" />);
    expect(markup).toContain('data-slot="chat-textarea"');
    expect(markup).toContain('data-slot="chat-priority"');
    expect(markup).toContain('data-slot="chat-send"');
    expect(markup).toContain('Send');
  });

  test('while streaming the send button is the interrupt control', () => {
    const markup = renderToStaticMarkup(<ChatInputMorph defaultExpanded streaming draftKey="k" />);
    expect(markup).toContain('data-escalation="interrupt"');
    expect(markup).toContain('Interrupt');
  });
});

describe('escalation state machine', () => {
  test('pure steps: escalate climbs to kill, decay steps back to interrupt', () => {
    expect(escalate('interrupt')).toBe('shutdown');
    expect(escalate('shutdown')).toBe('kill');
    expect(escalate('kill')).toBe('kill');
    expect(decay('kill')).toBe('shutdown');
    expect(decay('shutdown')).toBe('interrupt');
    expect(decay('interrupt')).toBe('interrupt');
  });

  test('rapid presses fire interrupt → shutdown → kill', () => {
    jest.useFakeTimers();
    try {
      const machine = createEscalationMachine();
      expect(machine.press()).toBe('interrupt');
      expect(machine.press()).toBe('shutdown');
      expect(machine.press()).toBe('kill');
      // The ceiling holds under further hammering.
      expect(machine.press()).toBe('kill');
      machine.dispose();
    } finally {
      jest.useRealTimers();
    }
  });

  test('an armed level decays one step per idle window until rest', () => {
    jest.useFakeTimers();
    try {
      const levels: EscalationLevel[] = [];
      const machine = createEscalationMachine((level) => levels.push(level), 3000);

      machine.press();
      machine.press();
      machine.press();
      expect(machine.level()).toBe('kill');

      jest.advanceTimersByTime(2999);
      expect(machine.level()).toBe('kill');
      jest.advanceTimersByTime(1);
      expect(machine.level()).toBe('shutdown');
      jest.advanceTimersByTime(3000);
      expect(machine.level()).toBe('interrupt');

      // At rest: no further timers, level stays put.
      jest.advanceTimersByTime(10_000);
      expect(machine.level()).toBe('interrupt');
      expect(levels).toEqual(['shutdown', 'kill', 'shutdown', 'interrupt']);
      machine.dispose();
    } finally {
      jest.useRealTimers();
    }
  });

  test('a press inside the window re-arms the decay clock', () => {
    jest.useFakeTimers();
    try {
      const machine = createEscalationMachine(undefined, 3000);
      machine.press();
      expect(machine.level()).toBe('shutdown');

      jest.advanceTimersByTime(2000);
      expect(machine.press()).toBe('shutdown');
      expect(machine.level()).toBe('kill');

      // The old timer must not fire early: only a full idle window decays.
      jest.advanceTimersByTime(2999);
      expect(machine.level()).toBe('kill');
      jest.advanceTimersByTime(1);
      expect(machine.level()).toBe('shutdown');
      machine.dispose();
    } finally {
      jest.useRealTimers();
    }
  });

  test('reset snaps to rest and dispose stops the decay clock', () => {
    jest.useFakeTimers();
    try {
      const machine = createEscalationMachine(undefined, 3000);
      machine.press();
      machine.press();
      machine.reset();
      expect(machine.level()).toBe('interrupt');

      machine.press();
      machine.dispose();
      jest.advanceTimersByTime(10_000);
      // Disposed: the armed level is frozen, no decay fires.
      expect(machine.level()).toBe('shutdown');
    } finally {
      jest.useRealTimers();
    }
  });
});
