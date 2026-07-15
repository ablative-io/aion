# B1 — AWL authoring ergonomics

Contract: `docs/design/aion-authoring/awl/AWL-FLOW-VOCABULARY.md` rev 3
§4. Crate: `crates/aion-awl` (lexer, parser, ast, printer, checker,
semantic index) plus `aion awl` CLI surfacing.

## Objective

Land the four ergonomics features so no AWL author ever writes an
escaped-JSON string or repeats a prompt literal again:

1. **Raw strings** — triple-quoted `"""…"""`: newlines literal, zero
   escape processing. Usable anywhere a string literal is.
2. **`json { … }` literals** — lexer captures the balanced-brace body
   verbatim (brace counting must respect braces inside JSON strings);
   the checker parses the body as JSON and rejects invalid JSON with a
   span-accurate diagnostic pointing INTO the body. Value type:
   `String`, the verbatim body text.
3. **`const` declarations** — document-level: `const name = <value>`
   where value is any literal (including raw strings and `json {}`),
   `schema of Type`, a list literal of these, or `+` concatenations of
   these — folded at compile time. Const names resolve anywhere an
   expression is legal. Checker: duplicate names rejected, undefined
   references rejected, no cycles, all with spans. Semantic index gains
   a `const` declaration kind (hover shows the folded value's type).
4. **`schema of Type`** — expression yielding the type's JSON Schema as
   a compile-time `String`, via the existing `schema::derive` module
   (which already maps `///` docs → `description` and `?` → optional).
   Works for any document-declared record/enum type.

Also: fix the parser so a statement may START with any expression —
today `"literal" -> name` fails with "expected a statement, found a
string literal" (this is why dev_flow has one schema pasted four
times).

## Scope out

No new step kinds, no `subflow`/`distribute`/`collect` (B2). No
emitter changes beyond making the new expressions reach existing
lowering as folded strings — `aion awl emit` on a document using all
four features must produce a Gleam project that `gleam build`s.

## The bar

- Every new construct round-trips the lossless canonical printer:
  parse → print → parse yields an identical tree INCLUDING comments and
  the verbatim `json {}` body. Property tests for the round-trip.
- Span-correct diagnostics for every new error class (invalid JSON,
  const cycles, undefined const, unterminated raw string) — asserted in
  tests by line:column.
- `aion awl check` and `aion awl fmt` handle all features; fmt is
  idempotent on them.
- One real compile proof: a document exercising raw strings, json
  literal, const folding, and `schema of` passes check, emits, and
  `gleam build`s clean. Keep it as a test fixture.
- Gates: cargo fmt / build / clippy -D warnings / test for the
  workspace, exit codes recorded.
