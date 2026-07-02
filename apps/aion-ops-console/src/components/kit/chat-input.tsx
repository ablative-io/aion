import { CornerDownLeft, MessageSquare } from 'lucide-react';
import { AnimatePresence, motion, useReducedMotion } from 'motion/react';
import { useEffect, useRef, useState } from 'react';

import { cn } from '@/lib/utils';

import { readChatDraft, writeChatDraft } from './chat-drafts';
import type { EscalationLevel, EscalationMachine } from './chat-escalation';
import { createEscalationMachine, ESCALATION_DECAY_MS } from './chat-escalation';
import { degradeToFade, SPRING_SIGNATURE } from './springs';
import type { KitStatus } from './status-dot';
import { KIT_ACCENT, StatusDot } from './status-dot';

// The deliberately LIGHT chat morph (design language §kit-1): a slim docked
// pill that spring-morphs into a well-proportioned textarea and nothing more.
// No profile/model/settings pills — intervention is almost the fallback here.

export type ChatPriority = 'queued' | 'interrupt';

export type ChatInputMorphProps = {
  /** Keys the module-level draft store so each conversation keeps its own draft. */
  draftKey: string;
  placeholder?: string;
  status?: KitStatus;
  /** Small capability chips shown on the docked pill (e.g. "interrupt", "queue"). */
  capabilities?: readonly string[];
  /** While true the send button becomes the interrupt/escalation control. */
  streaming?: boolean;
  expanded?: boolean;
  defaultExpanded?: boolean;
  onExpandedChange?: (expanded: boolean) => void;
  onSubmit?: (message: string, priority: ChatPriority) => void;
  /** Fired for escalation presses: interrupt (without text), shutdown, kill. */
  onEscalate?: (action: EscalationLevel) => void;
  /** Test/demo hook for the 3s escalation decay window. */
  escalationDecayMs?: number;
  className?: string;
};

const ESCALATION_LABEL: Record<EscalationLevel, string> = {
  interrupt: 'Interrupt',
  shutdown: 'Shutdown',
  kill: 'Kill',
};

// Distinct visual states per escalation level; red family reserved for kill.
const ESCALATION_CLASS: Record<EscalationLevel, string> = {
  interrupt: 'text-[var(--surface-base,#0f0f14)]',
  shutdown: 'bg-[var(--status-running,#fbbf24)] text-[var(--surface-base,#0f0f14)]',
  kill: 'bg-[var(--status-failed,#f87171)] text-[var(--surface-base,#0f0f14)] ring-2 ring-[var(--status-failed-glow,rgba(248,113,113,0.12))]',
};

export function ChatInputMorph({
  draftKey,
  placeholder = 'Message the agent…',
  status = 'idle',
  capabilities,
  streaming = false,
  expanded,
  defaultExpanded = false,
  onExpandedChange,
  onSubmit,
  onEscalate,
  escalationDecayMs = ESCALATION_DECAY_MS,
  className,
}: ChatInputMorphProps) {
  const reducedMotion = useReducedMotion() ?? false;
  const transition = degradeToFade(SPRING_SIGNATURE, reducedMotion);

  const [internalExpanded, setInternalExpanded] = useState(defaultExpanded);
  const isExpanded = expanded ?? internalExpanded;
  const setExpanded = (next: boolean) => {
    if (expanded === undefined) setInternalExpanded(next);
    onExpandedChange?.(next);
  };

  const [value, setValue] = useState(() => readChatDraft(draftKey));
  const [priority, setPriority] = useState<ChatPriority>('queued');
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  // Re-read the persisted draft when the conversation changes.
  useEffect(() => {
    setValue(readChatDraft(draftKey));
  }, [draftKey]);

  useEffect(() => {
    if (isExpanded) textareaRef.current?.focus();
  }, [isExpanded]);

  const [escalation, setEscalation] = useState<EscalationLevel>('interrupt');
  const machineRef = useRef<EscalationMachine | null>(null);
  useEffect(() => {
    if (!streaming) return;
    const machine = createEscalationMachine(setEscalation, escalationDecayMs);
    machineRef.current = machine;
    return () => {
      machine.dispose();
      machineRef.current = null;
      setEscalation('interrupt');
    };
  }, [streaming, escalationDecayMs]);

  const updateValue = (next: string) => {
    setValue(next);
    // Write-through so collapse/unmount can never eat the draft.
    writeChatDraft(draftKey, next);
  };

  const submitMessage = (message: string, messagePriority: ChatPriority) => {
    onSubmit?.(message, messagePriority);
    updateValue('');
  };

  const handleSend = () => {
    const message = value.trim();
    const machine = machineRef.current;
    if (streaming && machine) {
      const fired = machine.press();
      if (fired === 'interrupt' && message !== '') {
        submitMessage(message, 'interrupt');
      } else {
        onEscalate?.(fired);
      }
      return;
    }
    if (message === '') return;
    submitMessage(message, priority);
  };

  const sendLabel = streaming ? ESCALATION_LABEL[escalation] : 'Send';

  return (
    <motion.div
      layout={!reducedMotion}
      transition={transition}
      className={cn(
        'overflow-hidden border border-[var(--border-default)] bg-[var(--surface-elevated,#16161d)]',
        isExpanded ? 'rounded-xl shadow-lg' : 'h-9 rounded-full hover:bg-[var(--surface-hover)]',
        className
      )}
      data-slot="chat-input"
      data-expanded={isExpanded}
      data-streaming={streaming || undefined}
    >
      <AnimatePresence initial={false} mode="popLayout">
        {isExpanded ? (
          <motion.div
            key="editor"
            layout={!reducedMotion}
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0, filter: 'blur(10px)' }}
            transition={{ duration: 0.2, ease: 'easeOut' }}
            className="flex flex-col gap-2 p-3"
          >
            <textarea
              ref={textareaRef}
              value={value}
              onChange={(event) => updateValue(event.target.value)}
              onKeyDown={(event) => {
                // Element-scoped editor keys, not global hotkeys (those go
                // through the central registry). ⌘/Super is the primary
                // modifier — never Control.
                if (event.key === 'Enter' && event.metaKey) {
                  event.preventDefault();
                  handleSend();
                } else if (event.key === 'Escape') {
                  event.preventDefault();
                  setExpanded(false);
                }
              }}
              placeholder={placeholder}
              rows={3}
              className="w-full resize-none bg-transparent text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-muted)]"
              data-slot="chat-textarea"
            />
            <div className="flex items-center gap-2">
              <PriorityToggle priority={priority} onPriorityChange={setPriority} />
              <span className="flex-1 text-[10px] uppercase tracking-[0.15em] text-[var(--text-muted)]">
                ⌘↵ send · esc collapse
              </span>
              <button
                type="button"
                onClick={handleSend}
                className={cn(
                  'inline-flex h-8 items-center gap-1.5 rounded-lg px-3 text-xs font-semibold transition-colors duration-150',
                  'outline-none focus-visible:ring-2 focus-visible:ring-[var(--border-focus)]',
                  streaming ? ESCALATION_CLASS[escalation] : 'text-[var(--surface-base,#0f0f14)]'
                )}
                style={
                  streaming && escalation !== 'interrupt'
                    ? undefined
                    : { backgroundColor: KIT_ACCENT }
                }
                data-slot="chat-send"
                data-escalation={streaming ? escalation : undefined}
              >
                {sendLabel}
                <CornerDownLeft aria-hidden="true" className="size-3" />
              </button>
            </div>
          </motion.div>
        ) : (
          <motion.button
            key="pill"
            type="button"
            layout={!reducedMotion}
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0, filter: 'blur(10px)' }}
            transition={{ duration: 0.2, ease: 'easeOut' }}
            onClick={() => setExpanded(true)}
            className="flex h-9 w-full items-center gap-2 px-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-[var(--border-focus)]"
            data-slot="chat-pill"
          >
            <StatusDot status={status} pulse={status === 'live'} />
            <MessageSquare aria-hidden="true" className="size-3.5 text-[var(--text-muted)]" />
            <span className="min-w-0 flex-1 truncate text-xs text-[var(--text-muted)]">
              {value.trim() === '' ? placeholder : value}
            </span>
            {capabilities?.map((capability) => (
              <span
                key={capability}
                className="rounded-full border border-[var(--border-subtle,rgba(255,255,255,0.04))] bg-[var(--surface-hover)] px-2 py-0.5 text-[10px] uppercase tracking-[0.15em] text-[var(--text-secondary)]"
                data-slot="chat-capability"
              >
                {capability}
              </span>
            ))}
          </motion.button>
        )}
      </AnimatePresence>
    </motion.div>
  );
}

function PriorityToggle({
  priority,
  onPriorityChange,
}: {
  priority: ChatPriority;
  onPriorityChange: (priority: ChatPriority) => void;
}) {
  return (
    <fieldset
      className="inline-flex items-center rounded-lg border border-[var(--border-default)] p-0.5"
      aria-label="Delivery priority"
      data-slot="chat-priority"
    >
      {(['queued', 'interrupt'] satisfies ChatPriority[]).map((option) => (
        <button
          key={option}
          type="button"
          aria-pressed={priority === option}
          onClick={() => onPriorityChange(option)}
          className={cn(
            'rounded-md px-2 py-1 text-[10px] font-medium uppercase tracking-[0.15em] transition-colors duration-150',
            priority === option
              ? 'bg-[var(--surface-hover)] text-[var(--text-primary)]'
              : 'text-[var(--text-muted)] hover:text-[var(--text-secondary)]'
          )}
        >
          {option}
        </button>
      ))}
    </fieldset>
  );
}
