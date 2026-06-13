# Review Discipline

Review is adversarial verification, not collegial summary. The reviewer's
job is to find where the implementation diverges from the spec, the
decisions, and the codebase's standards — and either fix it or fail it.
A review that returns only praise is treated as a review that didn't look.

## The Standard

Would you trust this code with patient records, financial transactions,
or legal documents? If not, it is not ready. This is the bar for every
brief, every template, every fix — there is no "it's just a small one."

## No Minor Issues

There is no severity taxonomy in the review schema, deliberately. Every
finding is either **fixed in the harden pass** or **recorded as an issue**
that blocks alignment. "Minor" is the word that smuggles defects past the
gate; a naming drift, a missing doc comment, and a swallowed error are all
just issues. Everything is dealt with, nothing deferred, nothing skipped.

## Verify the Diff, Not the Report

The dev report is testimony; the diff is evidence. The reviewer:

- reads the actual changes (`git diff` against the round's base), file by
  file — not the `files_changed` list;
- executes each acceptance criterion against the code and records
  **evidence** (file:line, test name, command output) per criterion. One
  boolean per requirement is not a review — the schema requires a verdict
  per criterion;
- runs the brief's verification steps and records pass/fail per step;
- checks every consequence of every ADR in the brief's `design_anchor`
  against the change.

## Attestation vs. Gate

The dev attests (`tests_pass: true`); the workflow measures (the gate
activity runs fmt, clippy `-D warnings`, and the affected test suites
firsthand). Both are recorded in the brief's execution block. **The
attestation is never the gate.** When they diverge — the agent believed
tests passed and they didn't — the divergence itself is a finding: it
means the agent's verification habits drifted, and every other claim in
its report deserves extra suspicion on this run.

## Hardening

The reviewer fixes what it finds — directly, in the same session — and
records every fix alongside the issue it answers. Rules:

- Fixes are corrections toward the spec, never scope expansion. A
  reviewer adding features is drift wearing a reviewer's badge.
- After hardening, the gate runs again. A harden pass that breaks the
  build is caught by the workflow, not discovered at land.
- `alignment: "fixed"` means drifted-then-corrected; `"drifted"` with an
  empty fix list means the reviewer is failing the requirement loudly.
  Both are honest; silently upgrading drifted to aligned is the one
  dishonest move the schema cannot catch — which is why reviews are
  spot-audited against their evidence fields.

## Bypasses Are Findings

`#[allow(...)]`, `#[expect(...)]`, `#[ignore]`, `_var` renames, dead-code
`#[cfg]` tricks, empty catch arms, `let _ =` on fallible results — these
are not style preferences, they are gate bypasses, and any new one in a
diff is an automatic issue regardless of justification offered in the dev
report. Tests that need a runtime resource gate at runtime and log the
skip; they do not `#[ignore]`.

## What the Reviewer Receives

The full enrichment chain (spec + scout + dev per requirement), the
attestation, the measured gate results, the bound ADRs with their
consequences, and the boundaries. The reviewer has file tools and the
repo — the projection orients, the reviewer explores beyond it. Reviewing
only what the prompt contains is reviewing the dev's framing of the work;
the standard requires reviewing the work.
