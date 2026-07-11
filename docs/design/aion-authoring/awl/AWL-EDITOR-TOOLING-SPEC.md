# AWL editor tooling — tree-sitter grammar + LSP server (Lane B spec)

Status: READY FOR EXECUTION. Authored 2026-07-11 for delegated build (norn
agents / external hands); the operator (Fable session) reviews and merges.
Source language: `AWL-2-SPEC.md` (rev-2, frozen; rulings landed `1fab6e1d`).
Runs fully in parallel with the AWL-BC build (Lane A) — see the coordination
rules, which exist so that stays true.

## Mission

Two independent deliverables that make `.awl` pleasant to write in a real
editor:

1. **tree-sitter-awl** — a tree-sitter grammar for syntax highlighting and
   structural editing. Highlighting ONLY: it is never a diagnostic surface.
2. **`aion awl lsp`** — an LSP server built into the aion binary, as a THIN
   protocol adapter over the one real front end (`crates/aion-awl`).
   Diagnostics come from the real parser/checker; formatting from the real
   canonical printer. The LSP layer contains zero language logic.

## Non-negotiables

- **N1 — one checker.** The LSP never reimplements lexing, parsing, checking,
  or printing. It calls `aion_awl::{parse, check, print}` (already `pub` via
  `crates/aion-awl/src/lib.rs`). If something needs a new query surface on
  `aion-awl` (e.g. type-at-position for hover), STOP and file the request to
  the operator — do not fork logic into the LSP, and do not modify `aion-awl`
  yourself (see N2). A diagnostic the CLI and the LSP disagree on is a
  blocking defect.
- **N2 — `crates/aion-awl` is read-only for this lane.** The AWL-BC build
  (Lane A) is actively rewiring that crate. Lane B consumes its public API
  and touches nothing inside it. Same for `gleam/aion_flow` and
  `crates/aion-package`.
- **N3 — tree-sitter is presentation, not truth.** The grammar may be looser
  than the real parser (tree-sitter is error-tolerant by design; that is a
  feature for highlighting-while-typing). It must never be advertised or
  wired as a checker. No diagnostics, no "invalid" styling beyond tree-sitter
  ERROR nodes doing their natural thing.
- **N4 — the fixture corpus is the oracle.**
  `crates/aion-awl/tests/fixtures/rev2/` holds 164 fixtures; invalid ones
  carry 3-line `.expected` sidecars (`PARSE|CHECK` / message substring /
  1-based span line). Valid fixtures define what must parse and produce zero
  diagnostics; the sidecars define what the LSP must report and where.
- **Standing conventions** (aion-wide): files ≤500 code lines; `mod.rs` =
  re-exports only; no `unwrap()`/`expect()` outside tests; no
  `#[allow]`/`#[expect]`/`#[ignore]` in production code; no silent failures;
  format with `cargo fmt` and verify via `git status --porcelain` (never a
  format-check command); never pipe cargo output through grep/tail —
  redirect to a file and echo the exit code.

## Workstream TS — tree-sitter-awl

**Location:** `tools/tree-sitter-awl/` in the aion repo (grammar.js, queries,
committed generated `src/parser.c` so consumers never need the tree-sitter
CLI; pin the tree-sitter CLI version in the README and package.json).

**TS-1 — grammar + corpus conformance.**
- Encode the rev-2 surface from `AWL-2-SPEC.md`: the document structure
  (workflow header, `input`/`outcome` decls, `type` decls incl. the three
  doors — shorthand bodies, `= schema { … raw JSON … }`, `= schema("file")`
  — `worker` blocks with `action` lines and config, `step`s with `after`,
  substeps, statements: pipes `|>` with stages (field access, calls,
  combinators `filter/map/sort/count`), `->` bindings, `route`,
  `fork…join ->`, `loop … counting … until … max`, `wait … timeout`,
  `sleep`, `on failure`, conditional `outcome … when …` / `otherwise`
  clauses, doc lines `//!` and `///`, comments `//`, `?` optionality, `[T]`
  lists, string/number/duration/bool literals.
- Indentation matters in AWL (the lexer emits indentation tokens): use
  tree-sitter's `externals` scanner ONLY if genuinely required; first try a
  newline/indent-tolerant grammar that keys structure off keywords — the
  surface was designed so every construct opens with a keyword. Document
  whichever route is taken and why.
- **Gate TS-G1 (corpus sweep):** a script (`tools/tree-sitter-awl/test/corpus-sweep.sh`)
  that runs `tree-sitter parse` over EVERY valid fixture in
  `crates/aion-awl/tests/fixtures/rev2/` and both flagship examples
  (`docs/design/aion-authoring/awl/examples/rev2/*.awl`,
  `examples/awl-hello/awl_hello.awl`) and fails on any ERROR or MISSING
  node. Invalid fixtures are exempt (N3).
- **Gate TS-G2:** `tree-sitter test` green over a native test suite covering
  each construct (one named test per spec section minimum).

**TS-2 — queries.**
- `queries/highlights.scm`: keywords, types (PascalCase refs + builtins),
  action/step/worker names, strings, numbers, durations, comments vs doc
  lines (distinct capture for `//!`/`///`), operators (`|>`, `->`, `?`),
  combinators.
- `queries/folds.scm` (steps, worker blocks, type bodies, forks, loops) and
  `queries/indents.scm` (best-effort).
- **Gate TS-G3:** highlight smoke test — `tree-sitter highlight` runs clean
  over the flagship examples; a golden capture listing for `awl_hello.awl`
  is committed and asserted so capture drift is visible in review.
- README: wiring instructions for neovim (nvim-treesitter local install) and
  helix (`languages.toml` grammar source entry).

## Workstream LSP — `aion awl lsp`

**Location:** new crate `crates/aion-awl-lsp` (library + the loop), plus a
minimal wiring commit in `crates/aion-cli` adding the `awl lsp` subcommand
(the ONE permitted touch outside the new crate; keep it to a few lines in
`crates/aion-cli/src/awl.rs`).

**Transport/deps:** `lsp-server` + `lsp-types` (rust-analyzer lineage,
synchronous, minimal) over stdio. Full-document sync (`TextDocumentSyncKind::FULL`)
in v1 — `.awl` files are small; do not build incremental sync.

**LSP-1 — lifecycle + diagnostics + formatting.**
- `initialize`/`initialized`/`shutdown`/`exit`; capabilities advertise
  diagnostics (push), documentFormatting, and nothing else yet.
- On didOpen/didChange: run `aion_awl::parse`, then `check` when parse
  succeeds; publish diagnostics. Clear on didClose.
- **Position mapping (the one subtle bit):** `aion_awl::Span` carries
  zero-based BYTE offsets (`start`/`end`) plus 1-based line and 1-based
  CHARACTER column (`crates/aion-awl/src/lexer/tokens.rs`). LSP positions
  are 0-based line + UTF-16 code units. Convert from the byte offsets
  against the live document text (do not trust column arithmetic across
  multibyte); unit-test with multibyte fixtures (the corpus has unicode
  cases) and declare `positionEncoding` honestly if negotiating UTF-16.
- Diagnostic fields: severity Error, source `"awl"`, the checker/parser
  message verbatim (they are teaching-quality; do not rewrite them), range
  from the span (point range if end ≤ start).
- `textDocument/formatting`: run `aion_awl::parse` + `print`; if the
  document does not parse, return no edits (never a partial format). The
  result must be byte-identical to `aion awl fmt` — assert that in tests.
- **Gate LSP-G1 (corpus conformance):** an integration test that, for EVERY
  valid rev-2 fixture, yields zero diagnostics; and for EVERY invalid
  fixture with a sidecar, yields ≥1 diagnostic whose message contains the
  sidecar substring and whose start line (converted back to 1-based)
  matches the sidecar line. This reuses the exact contract the CLI tests
  pin — the two surfaces cannot drift.
- **Gate LSP-G2:** formatting test — for every valid fixture, LSP formatting
  output == `print(parse(text))` byte-for-byte, and a second format is a
  no-op (idempotence).

**LSP-2 — navigation niceties (still thin).**
- `textDocument/documentSymbol`: outline from the AST (workflow, types,
  workers/actions, steps/substeps) — pure AST walk, no new aion-awl API
  needed.
- `textDocument/definition` for names resolvable by AST walk alone (step
  refs in `route`/`after`, type refs, action refs to their `worker` decl
  line). If resolution would require checker internals, deliver what the
  AST gives and list the rest in the completion report (N1 escalation).
- Hover: DEFERRED unless `aion-awl` already exposes what it needs — v1
  ships without hover rather than growing a shadow type engine.
- README: helix `languages.toml` entry (`command = "aion", args = ["awl", "lsp"]`)
  — that is the fastest live test; plus a neovim `vim.lsp.start` snippet.
  VS Code extension: OUT of scope (follow-up item).

**Gate battery (both LSP phases, bare, exit codes recorded):**
`cargo fmt --all` + clean porcelain; `cargo clippy -p aion-awl-lsp -p aion-cli
--all-targets -- -D warnings`; `cargo test -p aion-awl-lsp`;
`cargo test -p aion-cli`. Plus one LIVE smoke: drive the built binary over
stdio with a scripted initialize→didOpen(bad file)→expect diagnostic→
formatting→shutdown exchange (this can be a test using `lsp-server`'s
in-process harness or a small pty script — either way it must actually run).

## Delivery protocol

- Branches off aion `main`: `awl-tooling-ts` and `awl-tooling-lsp`
  (independent merges; do not stack them). Work in worktrees
  (`.yggdrasil-worktrees/<branch>`), never in the main checkout.
- Commit style: small, explicit-path staging (never `git add -A`).
- No pushes to `main`, no merges — the operator reviews each branch (diff +
  bare gate re-run + live probe against the corpus) and merges `--no-ff`.
- Completion report per branch: gates with exit codes, corpus-sweep counts
  (files parsed / diagnostics matched), deviations from this spec with
  reasons, and anything hit that needed an `aion-awl` API that does not
  exist (N1 list).
- Sequencing within the lane: TS-1 → TS-2 and LSP-1 → LSP-2, but the two
  workstreams are fully parallel with each other.

## Explicitly out of scope (follow-ups, do not start)

VS Code extension packaging; hover/type-at-position (needs an `aion-awl`
query API — operator-owned); the mock/example/live "liveness dial" run
integration (lands after AWL-BC per the agreed roadmap); semantic tokens;
completion; rename; code actions; watch-mode rebuild (`aion dev`, #215
territory); any change to the language, printer, or checker.
