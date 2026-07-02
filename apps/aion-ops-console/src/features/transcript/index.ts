// `attemptId`/`resolveSelectedAttempt` are shared with the assistant panel so it
// can drive the SAME transcript machinery without forking the view.
export {
  AttemptTranscriptView,
  attemptId,
  resolveSelectedAttempt,
} from './components/AttemptTranscriptView';
export { InterventionControls } from './components/InterventionControls';
export { TranscriptEventRow } from './components/TranscriptEventRow';
export { TranscriptPanel, TranscriptPanelContent } from './components/TranscriptPanel';
export { sortAttempts, useActivityAttempts } from './hooks/useActivityAttempts';
export { useIntervene } from './hooks/useIntervene';
export { foldTranscriptEvent, type TranscriptEntry, useTranscript } from './hooks/useTranscript';
