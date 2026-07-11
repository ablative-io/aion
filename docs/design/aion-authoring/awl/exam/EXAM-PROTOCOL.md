# The AWL exam — protocol

Tom's design (2026-07-11), operationalized. The exam measures whether AWL +
one page of docs is enough for a model that has never seen the language to
author a correct workflow — and it measures the DOCS and the LANGUAGE as much
as the candidates. Repeated failures on the same point are a spec bug, a docs
gap, or a bad error message before they are a model gap; the fix is the error
message, not a fatter candidate pack.

## Ground rules

- Turn 1 gives the candidate ONLY `CANDIDATE-PACK.md` and the task inside
  it. No feedback schema, no grading criteria, no AWL spec, no corpus —
  showing the rubric up front changes what they write.
- The invigilator (not the candidate) runs `aion awl check`, then the full
  `emit` + `gleam build` chain, and — once the server is back — a live run.
  Turn-1 candidates need only read+write in a scratch directory. Nothing
  else. No network, no server, no repo access.
- Turn 2: the candidate is shown what happened (check output verbatim,
  build/run result) and asked for structured feedback against
  `feedback.schema.json`. This is where "what confused you / what did you
  reach for that didn't exist" gets captured — after the attempt, so it
  reflects the real experience rather than performance for the grader.
- Every sitting is recorded in `LEDGER.md` — including the invigilator's
  own pipeline-proof sitting, marked as such.

## The matrix

harness × model × reasoning effort. First sitting:

| Harness | Models | Efforts |
|---|---|---|
| norn (GPT) | gpt-5.6-sol | medium, high, xhigh |
| Claude Code (headless) | opus, sonnet, haiku | default |
| bare harness (Tom's, when it exists) | TBD | TBD |

Fable sits sparingly (usage discipline) — one sitting as invigilator
pipeline-proof only. The bare harness (ten-word system prompt, three tools)
is the purest test of whether the LANGUAGE carries the load; treat its
column as first-class when it arrives.

Candidates run in parallel; each gets a fresh scratch directory containing
only `CANDIDATE-PACK.md`. The candidate's prompt is the same short
instruction for everyone: "Read CANDIDATE-PACK.md and complete the task.
Write order_followup.awl in this directory. Say DONE when you are
satisfied."

## Marking (the 2B pencil)

Per sitting, four marks plus notes:

1. **check-pass**: did `aion awl check` pass on the FIRST submitted file?
   (first_try | after_N_fixes | never)
2. **semantic**: does the workflow do what the task says? Rubric = the six
   numbered requirements on the task card, each pass/fail (retry config
   exactly as asked; sleep present and 24h; second fetch actually re-fetches;
   conditional outcomes routed correctly with the required payloads).
3. **stall points**: where did they visibly hesitate, re-read, or invent —
   from the transcript.
4. **legal-but-ugly**: constructs that pass the checker but a fluent author
   would not write (noted verbatim — these seed style docs and lints).

Grade inflation guard: semantic marking is against the task card text, not
against "close enough". A missing retry spacing is a fail on requirement 1.

## What feeds back where

- check failed on syntax the pack shows → pack wording bug (fix the pack —
  the ONE permitted kind of pack edit is clarifying something it already
  tries to teach; adding new material is not permitted).
- check failed on syntax the pack doesn't show → language/docs decision:
  should the task need it? If yes, spec/pack gap; if no, task bug.
- confusing checker error (candidate flailed after reading it) → error
  message bug, file an aion-awl issue with the transcript excerpt.
- turn-2 "missing from language" hits that recur across gene pools →
  candidate input to the AWL advisory backlog (retry semantics ruling etc.).

## Automation note

First sitting is run by hand (invigilator = Vesper, scripts + norn skill +
headless claude). If the exam earns its keep it becomes a workflow: N×M
fan-out, per-candidate scratch provisioning, check/emit/build gates,
turn-2 collection, ledger append — the exam becomes a standing regression
suite for the language itself, run on every spec change.
