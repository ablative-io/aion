# F — TypeScript subset (real `.ts`, statically extracted, never executed)

Added after the AI-authorship constraint landed (Tom, 2026-07-02): *the primary workflow
authors will be AI agents, so a bespoke lookalike syntax is a friction point — we want
the syntax, not the language.* This flavour takes that literally: the workflow file **is
TypeScript** — parsed by a real TS parser, typechecked by `tsc` — but **never executed**.
There is no Node runtime. `aion check` accepts a defined subset, extracts the canonical
graph, and the engine runs Gleam underneath, same as every other flavour.

Precedents: AssemblyScript (TS syntax, non-JS semantics), Encore.ts (static TS analysis
drives codegen), Temporal TS (authoring workflows in TS — but they execute it and police
determinism at runtime; we make the stronger static move).

```ts
import { workflow, fanOut, waitFor } from "aion";
import * as act from "./actions";        // .d.ts GENERATED from the action contracts

export default workflow("research_report", async ({ brief, corpus }, { review }) => {
  const questions = await act.plan(brief);

  const findings = await fanOut(questions, (q) => act.investigate(q, corpus), {
    retry: 3,
    backoff: "30s",
  });

  let draft = await act.synthesize(findings, "");

  const approval = await waitFor(review, {
    timeout: "3d",
    onTimeout: () => ({ report: draft, url: "" }),   // early-complete, unpublished
  });

  if (!approval.ok) {
    draft = await act.synthesize(findings, approval.notes);
  }

  const assets = await act.uploadAssets(draft);

  try {
    const url = await act.publish(draft, assets);
    return { report: draft, url };
  } catch (e) {
    await act.deleteAssets(assets);
    throw e;
  }
});
```

Every character is legal TypeScript. `tsc --noEmit` typechecks the wiring against the
generated ambient environment. Boundary types live in a generated `./types.d.ts`
(from the contract registry); input arrives already typed.

## Why this beats flavour A for AI authors (the decisive argument)

- **A's problem was plausible-but-wrong priors.** A JS-*lookalike* constantly pulls an
  LLM toward real JS, and every drift is a syntax error in a language that exists nowhere
  in its training data. Here the prior is simply *correct*: `await`, `let`, `if`,
  `try/catch`, `return` mean exactly what the model expects. `await` doubles as the
  visual marker for "durable step".
- **The guardrails are tsc diagnostics — the best self-correction signal an LLM can get.**
  The ambient env contains ONLY the actions + aion combinators: no DOM, no Node globals.
  `fetch(...)` is not a policed exception; it is `Cannot find name 'fetch'`. Wrong wiring
  (`act.synthesize(questions, …)`) is a plain type error naming both types. Models fix
  TS diagnostics in one round; so do humans.
- **Free tooling.** The file is `.ts`: LSP, syntax highlighting, prettier, editor
  refactors all work with zero effort from us.

## The subset (enforced by `aion check`, with named fixes)

Allowed: `const`/`let`, calls to imported actions/combinators, `await`, field access,
literals, arithmetic/comparison/boolean expressions, `if`/`else`, ternary, object
literals, `try/catch/throw`, `return`, arrow functions as combinator arguments.
Rejected with a named fix: everything else —
`questions.map(f)` → "use `fanOut(questions, f)` for durable parallel steps, or move the
computation into a helper"; `while` → "unbounded loops aren't representable; use a child
workflow with continue-as-new"; `Math.random()`/`Date.now()` → already `Cannot find name`.
Running `node workflow.ts` prints nothing useful — the aion package's runtime export
throws "workflows are compiled by aion, not executed by Node" for anyone who tries.

## The second AI lane this unlocks

Because the canonical model is JSON-with-a-schema, an agent whose harness supports
**structured output / constrained decoding can author the graph directly** — a
syntactically invalid workflow becomes *impossible to emit* — and the toolchain renders
the pretty `.ts` surface for human review. Lossless surfaces mean: AI writes whichever
lane its harness does best; humans always read the readable one.

## Honest costs

- **Subset enforcement engineering**: a real TS parser (SWC/OXC, Rust) + the subset
  linter + graph extractor. Bounded, front-end-only work; the engine is untouched.
- **Comment round-trip**: canvas edits regenerate the `.ts`; preserving comments means
  attaching them to graph nodes. Solvable (canonical printer), but it's real work.
- **Helpers don't live in this file.** The workflow file is orchestration only; pure
  helpers and actions live behind the import boundary (Gleam/Rust/Python), typed once,
  surfaced through the generated `.d.ts`. Anyone wanting computation *here* is redirected
  to a helper — same rule as every flavour, just enforced at a very visible seam.
- People will assume it executes like Temporal TS does. Docs must say early: *statically
  extracted, never run; that is why determinism is guaranteed rather than policed.*

## Runner-up noted: Starlark

If we ever want a Python-shaped surface: Starlark is a real existing language,
deterministic and I/O-free *by spec*, with a production Rust interpreter (Buck2's).
Maximum LLM syntax fluency; loses to the TS subset on the feedback loop (no static
types → wiring errors surface in our checker, not a typechecker the model already knows)
and on engine fit. Back pocket.

## Verdict

**Supersedes flavour A as the primary candidate.** A's entire cost was policing the
uncanny valley; making the file real TypeScript hands that job to tsc and turns our
biggest weakness for AI authors into the strongest feedback loop available.
