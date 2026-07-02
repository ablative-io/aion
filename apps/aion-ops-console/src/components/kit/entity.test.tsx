import { describe, expect, test } from 'bun:test';
import { renderToStaticMarkup } from 'react-dom/server';
import type { EntityForm } from './entity';
import {
  collapsedForm,
  createEntityKeyboardActions,
  ENTITY_FORMS,
  Entity,
  expandedForm,
} from './entity';

describe('entity form-factor helpers', () => {
  test('expand walks pill → card → window and stops at the ceiling', () => {
    expect(expandedForm('pill')).toBe('card');
    expect(expandedForm('card')).toBe('window');
    expect(expandedForm('window')).toBe('window');
  });

  test('collapse walks window → card → pill and stops at the floor', () => {
    expect(collapsedForm('window')).toBe('card');
    expect(collapsedForm('card')).toBe('pill');
    expect(collapsedForm('pill')).toBe('pill');
  });

  test('keyboard actions fire onFormChange only when the form actually changes', () => {
    const changes: EntityForm[] = [];
    const record = (form: EntityForm) => changes.push(form);

    createEntityKeyboardActions('pill', record).expand();
    createEntityKeyboardActions('card', record).expand();
    createEntityKeyboardActions('window', record).expand();
    createEntityKeyboardActions('window', record).collapse();
    createEntityKeyboardActions('pill', record).collapse();

    expect(changes).toEqual(['card', 'window', 'card']);
  });
});

describe('entity render states', () => {
  const renderForm = (form: EntityForm) =>
    renderToStaticMarkup(
      <Entity
        form={form}
        name="wf-faaa1b04"
        status="running"
        headlines={['reading transcript', 'writing plan']}
        header={<span>step 3/7</span>}
        quickInput={<input placeholder="nudge" />}
      >
        <p>transcript body</p>
      </Entity>
    );

  test('every form renders a distinct surface tagged with its form factor', () => {
    const markups = ENTITY_FORMS.map((form) => renderForm(form));
    expect(new Set(markups).size).toBe(ENTITY_FORMS.length);
    for (const form of ENTITY_FORMS) {
      expect(renderForm(form)).toContain(`data-form="${form}"`);
    }
  });

  test('pill form streams a headline and keeps the identity visible', () => {
    const markup = renderForm('pill');
    expect(markup).toContain('wf-faaa1b04');
    expect(markup).toContain('reading transcript');
    expect(markup).toContain('data-slot="entity-pill-stream"');
    expect(markup).not.toContain('transcript body');
    expect(markup).not.toContain('nudge');
  });

  test('card form renders header, children and the quick-input slot', () => {
    const markup = renderForm('card');
    expect(markup).toContain('step 3/7');
    expect(markup).toContain('transcript body');
    expect(markup).toContain('data-slot="entity-quick-input"');
    expect(markup).toContain('data-slot="entity-card-body"');
  });

  test('window form hands the full surface to children without the quick input', () => {
    const markup = renderForm('window');
    expect(markup).toContain('transcript body');
    expect(markup).toContain('data-slot="entity-window-body"');
    expect(markup).not.toContain('data-slot="entity-quick-input"');
  });

  test('status is communicated through the shared dot, never per-component', () => {
    expect(renderForm('pill')).toContain('data-slot="status-dot"');
    expect(renderForm('pill')).toContain('data-status="running"');
  });
});
