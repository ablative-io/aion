// The motion kit — Phase 0 "material" (OPS-CONSOLE-DESIGN-LANGUAGE.md).
// Purely additive: views adopt these primitives as they are (re)built.

export type { AnchoredMorph, AnchoredMorphOptions } from './anchored-morph';
export { useAnchoredMorph } from './anchored-morph';
export type { AnimatedBackgroundItemProps, AnimatedBackgroundProps } from './animated-background';
export { AnimatedBackground } from './animated-background';
export { clearChatDraft, readChatDraft, writeChatDraft } from './chat-drafts';
export type { EscalationLevel, EscalationMachine } from './chat-escalation';
export {
  createEscalationMachine,
  decay,
  ESCALATION_DECAY_MS,
  ESCALATION_ORDER,
  escalate,
} from './chat-escalation';
export type { ChatInputMorphProps, ChatPriority } from './chat-input';
export { ChatInputMorph } from './chat-input';
export type { DisclosureProps } from './disclosure';
export { Disclosure, DisclosureContent, DisclosureTrigger } from './disclosure';
export type { EntityForm, EntityKeyboardActions, EntityProps } from './entity';
export {
  collapsedForm,
  createEntityKeyboardActions,
  ENTITY_FORMS,
  Entity,
  EntityPillStream,
  expandedForm,
} from './entity';
export type { ExpandableRowProps } from './expandable-row';
export { ExpandableRow, resolveExpanded } from './expandable-row';
export type { FloatingPanelRootProps } from './floating-panel';
export {
  FloatingPanelBody,
  FloatingPanelContent,
  FloatingPanelFooter,
  FloatingPanelRoot,
  FloatingPanelTrigger,
} from './floating-panel';
export type { MorphingPopoverProps } from './morphing-popover';
export {
  MorphingPopover,
  MorphingPopoverContent,
  MorphingPopoverTrigger,
} from './morphing-popover';
export type { SlidingNumberParts, SlidingNumberProps } from './sliding-number';
export { SlidingNumber, splitSlidingNumber } from './sliding-number';
export {
  degradeToFade,
  MICRO_TRANSITION,
  MICRO_TRANSITION_SLOW,
  REDUCED_MOTION_FADE,
  SPRING_SECONDARY,
  SPRING_SIGNATURE,
  SPRING_SUCCESS,
  useReducedMotionTransition,
} from './springs';
export type { KitStatus, StatusDotProps } from './status-dot';
export {
  KIT_ACCENT,
  KIT_ACCENT_GLOW,
  KIT_STATUS_COLOR,
  KIT_STATUS_GLOW,
  StatusDot,
} from './status-dot';
export type { TransitionPanelProps } from './transition-panel';
export { TransitionPanel } from './transition-panel';
