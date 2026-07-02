import { useId } from 'react';

import { Button } from '@/components/ui';

import type { EventSearchFormState } from '../hooks/useEventSearch';

export type SearchFormProps = {
  value: EventSearchFormState;
  onChange: (next: EventSearchFormState) => void;
  onSubmit: () => void;
  onClear: () => void;
  /** When true, the submit button is disabled and the empty-query hint shows. */
  isEmpty: boolean;
  isLoading: boolean;
  /** Ref to the first input, so the view's focus-search action can target it. */
  firstFieldRef?: React.Ref<HTMLInputElement> | undefined;
};

const FIELD_CLASS =
  'h-9 rounded-md border border-input bg-transparent px-3 text-sm outline-none focus:border-[var(--text-muted)]';

/**
 * Field-aware event-search inputs (plan §4.5). All fields are optional and
 * AND-combined; submitting an all-blank form is blocked (`isEmpty`) with a visible
 * hint rather than silently running a match-all query.
 */
export function SearchForm({
  value,
  onChange,
  onSubmit,
  onClear,
  isEmpty,
  isLoading,
  firstFieldRef,
}: SearchFormProps) {
  const ids = useFieldIds();

  function handleSubmit(event: React.FormEvent) {
    event.preventDefault();
    onSubmit();
  }

  return (
    <form
      className="grid gap-3 rounded-lg border border-[var(--border-default)] p-4 md:grid-cols-3"
      onSubmit={handleSubmit}
    >
      <TextField
        id={ids.eventType}
        inputRef={firstFieldRef}
        label="Event type"
        placeholder="e.g. ActivityFailed"
        value={value.eventType}
        onChange={(next) => onChange({ ...value, eventType: next })}
      />
      <TextField
        id={ids.workflowType}
        label="Workflow type"
        placeholder="e.g. EmailDigest"
        value={value.workflowType}
        onChange={(next) => onChange({ ...value, workflowType: next })}
      />
      <TextField
        id={ids.activityType}
        label="Activity type"
        placeholder="e.g. SendEmail"
        value={value.activityType}
        onChange={(next) => onChange({ ...value, activityType: next })}
      />
      <TextField
        id={ids.errorText}
        label="Error text"
        placeholder="error message or kind"
        value={value.errorText}
        onChange={(next) => onChange({ ...value, errorText: next })}
      />
      <TextField
        id={ids.recordedAfter}
        label="Recorded after"
        type="datetime-local"
        value={value.recordedAfter}
        onChange={(next) => onChange({ ...value, recordedAfter: next })}
      />
      <TextField
        id={ids.recordedBefore}
        label="Recorded before"
        type="datetime-local"
        value={value.recordedBefore}
        onChange={(next) => onChange({ ...value, recordedBefore: next })}
      />
      <div className="flex items-end gap-2 md:col-span-3">
        <Button disabled={isEmpty || isLoading} type="submit">
          {isLoading ? 'Searching…' : 'Search'}
        </Button>
        <Button onClick={onClear} type="button" variant="outline">
          Clear
        </Button>
        {isEmpty ? (
          <p className="text-[var(--text-muted)] text-sm">
            Set at least one field to search; an empty query is not run.
          </p>
        ) : null}
      </div>
    </form>
  );
}

type TextFieldProps = {
  id: string;
  label: string;
  value: string;
  onChange: (next: string) => void;
  placeholder?: string;
  type?: 'text' | 'datetime-local';
  inputRef?: React.Ref<HTMLInputElement> | undefined;
};

function TextField({
  id,
  label,
  value,
  onChange,
  placeholder,
  type = 'text',
  inputRef,
}: TextFieldProps) {
  return (
    <label className="flex flex-col gap-2 text-sm font-medium" htmlFor={id}>
      {label}
      <input
        className={FIELD_CLASS}
        id={id}
        placeholder={placeholder}
        ref={inputRef}
        type={type}
        value={value}
        onChange={(event) => onChange(event.currentTarget.value)}
      />
    </label>
  );
}

function useFieldIds() {
  const prefix = useId();

  return {
    eventType: `${prefix}-event-type`,
    workflowType: `${prefix}-workflow-type`,
    activityType: `${prefix}-activity-type`,
    errorText: `${prefix}-error-text`,
    recordedAfter: `${prefix}-recorded-after`,
    recordedBefore: `${prefix}-recorded-before`,
  };
}
