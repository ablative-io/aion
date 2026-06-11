# Remediation briefs and design records

Working design briefs and decision records produced during the 2026-06 production-hardening remediation. These are narrative working documents — the formal design system (JSON source of truth, rendered markdown, coverage tooling) lives under `docs/design/` and is unchanged by anything here. When a brief's decisions harden into permanent contract, they belong in `docs/design/` / `CLIENT-CONTRACT.md`; these files record how and why the decisions were made.

| Brief | Task | Status |
|---|---|---|
| [websocket-resume.md](websocket-resume.md) | #37 | Decisions T1–T7 signed off; proto + engine publisher + Python/TS/Gleam client waves committed; server splice and Rust client waves remain. Carries the publisher review riders (namespace-aware filtering at the splice seam; cancellation-safety assumption on `PublishingEventStore::append`). |
| [schedule-namespace-gating.md](schedule-namespace-gating.md) | #32/#33 | Implemented and committed (`9f554017`). Design record. |
| [worker-reconnect-policy.md](worker-reconnect-policy.md) | #46 | Pair A approved; implementation wave in flight. Protocol drain signal deferred to the #39/#47 proto wave. |
| [query-execution.md](query-execution.md) | #45 | All nine decisions signed off (strict resolution); Wave 1 (engine core) in flight. |
| [child-await-two-phase.md](child-await-two-phase.md) | #58 (+#56, #55) | Designed; decisions adopted. Converts await_child/collect_* from dirty blocking NIFs to two-phase suspending natives; folds in #56 record-then-spawn and #55 cleanups; found D3 latent cursor NonDeterminism defect and D4 SDK/engine envelope mismatch. Waves A–E after #45 Wave 1. |
