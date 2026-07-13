# rev-2 fixture corpus — coverage ledger

One line per fixture. Later phases (lexer, parser/printer, checker, emitter) gate against this
ledger: every valid fixture must parse, print canonically (except where noted), and check clean;
every invalid fixture must fail at the sidecar's stage with a diagnostic containing the sidecar's
substring, primary span on the sidecar's 1-based line.

Sidecar format (`<name>.expected`, 3 lines): `PARSE|CHECK` / required message substring /
1-based line of the primary span. Spec of record:
`docs/design/aion-authoring/awl/AWL-2-SPEC.md`; decisions D1-D9 in `AWL-2-BUILD-PLAN.md`.

## flagship — the two spec examples, verbatim

- `flagship/valid/awl_hello.awl` — spec worked example 1, byte-identical; pipe chain with
  `.field` stage and `|> route` terminator, single worker, bare actions.
- `flagship/valid/dev_brief.awl` — spec worked example 2, byte-identical; file-import schema
  door, `String?`, doc'd type + field, `after` DAG diamond, unbound side-effect call,
  `loop = seed counting … until … max`, fork-over-collection + `join ->`, `filter(.field)`,
  `is empty`, route-targeted step, two-line canonical outcome break, `== 1` comparison in payload.
- `flagship/valid/schemas/brief.schema.json` — import target for `Brief` (object, optional
  `acceptance_criteria` array, descriptions).
- `flagship/valid/schemas/run_config.schema.json` — import target for `RunConfig` (nested
  `$defs`-local `$ref` for lenses; required string/array/integer fields matching every
  `config.*` use in dev_brief).

## header-types — narration, header decls, shorthand types, enums

Valid:
- `header-types/valid/minimal.awl` — smallest complete workflow: one input, one outcome, one type,
  one worker/action, pipe-route step.
- `header-types/valid/builtins.awl` — every builtin: `Dir`, `Float`, `Int`, `Bool`, `String`,
  `[String]`, `String?`, `[String]?`, `Nil` action result, composed optional-list field.
- `header-types/valid/zero_inputs.awl` — zero-input workflow (grammar requires only >=1 outcome).
- `header-types/valid/signal_wait.awl` — `signal` decl + `wait <signal> -> name`, two outcomes
  (success + failure), payload construction in route.
- `header-types/valid/enum.awl` — payload-less enum declared and used as a field type.
- `header-types/valid/doc_comments.awl` — multi-line `///` on types, `///` on fields
  (field docs force multi-line layout per golden precedent).
- `header-types/valid/line_width.awl` — 100-col rule: single-line type vs multi-line type with
  trailing commas.
- `header-types/valid/noncanonical_commas.awl` — comma tolerance (multi-line without trailing
  commas; single-line WITH trailing comma). Parse-accepts, fmt normalizes: EXCLUDE from
  byte-identity print goldens.
- `header-types/valid/combined.awl` — kitchen sink: multi-line `//!`, 4 inputs, signal, enum,
  doc'd type, durable wait, canonical two-line outcome break; plain `//` trivia comments at
  declaration level and statement level (the corpus's lossless-comment witnesses).

Invalid (stage / substring / span line):
- `missing_doc_header.awl` — no `//!` narration → PARSE "//!" @1 (judgment: grammar says one or
  more `//!` lines; re-stage to CHECK if the checker owns it).
- `doc_line_after_header.awl` — `//!` after header → PARSE "//!" @7.
- `no_outcomes.awl` — zero outcomes → PARSE "outcome" @2 (same judgment note as above).
- `input_missing_type.awl` — `input host` → PARSE "input" @3.
- `signal_missing_type.awl` — `signal cancel` → PARSE "signal" @4.
- `outcome_missing_type.awl` — outcome without `type T` → PARSE "type" @4.
- `outcome_route_invalid.awl` — `route sideways` (not success|failure) → PARSE "sideways" @4.
- `keyword_as_input_name.awl` — `input step:` (keyword reserved everywhere) → PARSE "step" @4.
- `unclosed_list_type.awl` — `[String` → PARSE "]" @4.
- `about_gone.awl` — gone keyword `about` → PARSE "about" @3 (pointed migration diagnostic).
- `option_gone.awl` — gone `Option(String)` → PARSE "Option" @4.
- `list_gone.awl` — gone `List(String)` → PARSE "List" @6.
- `finish_gone.awl` — gone `finish` statement (finishing IS routing) → PARSE "finish" @13.
- `enum_payload_variant.awl` — `Circle(Float)` payload variant deferred → PARSE "payload" @7.
- `duplicate_input.awl` — CHECK "duplicate input" @4 (span at second occurrence).
- `duplicate_signal.awl` — CHECK "duplicate signal" @5.
- `duplicate_outcome.awl` — CHECK "duplicate outcome" @5.
- `duplicate_type.awl` — CHECK "duplicate type" @7.
- `duplicate_field.awl` — CHECK "duplicate field" @6.
- `builtin_type_redeclared.awl` — `type Bool { … }` → CHECK "Bool" @7.
- `unknown_input_type.awl` — CHECK "unknown type" @4.
- `unknown_outcome_type.awl` — CHECK "unknown type" @4.
- `workflow_name_not_snake_case.awl` — CHECK "snake_case" @2.
- `type_name_not_title_case.awl` — CHECK "TitleCase" @7.
- `field_name_not_snake_case.awl` — CHECK "snake_case" @6.
- `enum_variant_not_title_case.awl` — CHECK "TitleCase" @7.
- `list_element_optional.awl` — `[String?]` field type in a shorthand type declaration (ruled
  2026-07-11: `?` illegal in list-element position) → CHECK "element" @6 (span on the element
  type).

## schema-doors — inline `schema {…}`, file import, `?` optionality

Valid (schema files live beside the .awl that imports them):
- `schema-doors/valid/inline_schema_round.awl` — inline raw-schema door (spec-verbatim Round
  shape) projected into guards and payloads.
- `schema-doors/valid/import_ticket.awl` (+ `ticket.schema.json`) — file import; absent-from-
  required property types as `T?`; `is present` flow-typing on a plain binding.
- `schema-doors/valid/import_nested_defs.awl` (+ `order.schema.json`) — nested objects, arrays,
  string enum compared with a string literal, `$defs`-local `$ref`, multi-level field access.
- `schema-doors/valid/import_constraints.awl` (+ `profile.schema.json`) — constraint keywords
  (minLength/maxLength/pattern/minimum/maximum/format) preserved but ignored for typing;
  string `+`.
- `schema-doors/valid/optional_shorthand.awl` — `?` shorthand with field `///` docs; payload
  construction both providing and omitting an optional field.
- `schema-doors/valid/mixed_doors.awl` (+ `intake.schema.json`) — all three doors in one
  document, inline schema with constraints, action config line, `count`, route-targeted step.
- `schema-doors/valid/inline_verbatim_constraints.awl` — the paste-verbatim promise: inline
  schema with a negative bound, `1e-3`/`1E5` exponent literals, `\uXXXX` and `\/` string
  escapes, braces inside a `pattern` string, and three-space indentation — all raw-captured
  by the lexer, preserved byte-for-byte on re-emit (constraints ignored for typing).

Invalid:
- `import_oneof.awl` (+ `oneof.schema.json`) — unsupported keyword → CHECK "oneOf" @6 (span at
  the import decl; keyword + JSON path belong in the message).
- `import_anyof.awl` (+ `anyof.schema.json`) — CHECK "anyOf" @6.
- `import_patternprops.awl` (+ `patternprops.schema.json`) — CHECK "patternProperties" @6.
- `import_external_ref.awl` (+ `extref.schema.json`) — non-`$defs`-local `$ref` → CHECK "$ref"
  @6 (inferred: spec supports only $defs-local refs).
- `import_missing_file.awl` — import target absent on disk → CHECK "nowhere.schema.json" @6.
- `import_unparseable.awl` (+ `broken.schema.json`, deliberately truncated JSON) → CHECK
  "broken.schema.json" @6.
- `import_null_type.awl` (+ `nullable.schema.json`) — `"type": "null"` property → CHECK "null"
  @6 (inferred: no null in the language).
- `inline_null_type.awl` — same rule through the inline door → CHECK "null" @11 (span at the
  offending JSON line inside the block).
- `inline_oneof.awl` — unsupported-keyword rule applied to the inline door → CHECK "oneOf" @12
  (judgment: three doors, one type system).
- `inline_bad_json.awl` — malformed inline JSON → PARSE "wibble" @8 (diagnostic quotes the
  offending lexeme).
- `construct_null_payload.awl` — `body: null` in payload construction (D4: absence by omission)
  → CHECK "null" @17 (judgment: build plan assigns null-rejection to phase 4; flip to PARSE with
  a one-line sidecar edit if implementation rejects earlier).

## declarations — worker/action config, child, spawn, call contracts

Valid:
- `declarations/valid/worker_single_action.awl` — bare action, no config line.
- `declarations/valid/worker_action_config_lines.awl` — `node+timeout` and
  `node+timeout+retry N every D` config lines.
- `declarations/valid/worker_retry_backoff.awl` — `retry N backoff D..D` range form.
- `declarations/valid/workers_multiple.awl` — two worker blocks in one document.
- `declarations/valid/child_call_awaited.awl` — `child` decl + awaited call with binding.
- `declarations/valid/spawn_detached.awl` — `spawn` fire-and-forget of a Nil-returning child.
- `declarations/valid/call_site_override.awl` — call-site `node`/`timeout` override (SYNTAX
  INFERRED: config line indented under the call statement — spec grants the override but shows
  no concrete form; rewrite mechanically if the real grammar differs).
- `declarations/valid/declarations_combined.awl` — 4 actions covering all config shapes incl.
  node-only and retry-only lines, 2 children, awaited call, spawn, unbound side-effect calls,
  call-site override, route-to-step + route-to-outcome.

Invalid:
- `positional_call_args.awl` — positional argument → PARSE "named" @13 (judgment: "unwritable"
  read as grammar-level; canonical fixture for the class, step-bodies duplicate removed).
- `missing_required_arg.awl` — required arg omitted → CHECK "channel" @13 (canonical; dup removed).
- `unknown_arg_name.awl` — undeclared arg name → CHECK "recipient" @13 (canonical; dup removed;
  may legitimately cascade a missing-arg secondary — substring pins the primary only).
- `duplicate_arg_name.awl` — same named arg twice → CHECK "duplicate" @13.
- `arg_type_mismatch.awl` — Int passed for String → CHECK "String" @13 (canonical; dup removed).
- `spawn_with_binding.awl` — `spawn … -> handle` → CHECK "spawn" @15 (the one stage the spec
  states explicitly).
- `spawn_of_action.awl` — spawn targets a worker action → CHECK "child" @14.
- `spawn_unknown_child.awl` — spawn of undeclared child → CHECK "audit_ghost" @13.
- `call_unknown_action.awl` — pipe into undeclared action → CHECK "frobnicate" @12.
- `child_inside_worker.awl` — child declared inside a worker block → PARSE "child" @11.
- `worker_without_actions.awl` — worker block with zero actions → PARSE "action" @8 (judgment:
  "one or more per worker" read as grammar).
- `retry_missing_schedule.awl` — `retry 2` without every/backoff → PARSE "every" @10.
- `retry_backoff_missing_range.awl` — backoff without `..max` → PARSE ".." @10.
- `timeout_missing_duration.awl` — bare `timeout` key → PARSE "duration" @10.
- `action_missing_return_type.awl` — no `-> Type` → PARSE "->" @9 (inferred mandatory: every
  spec example carries it and Nil exists for effect-only actions).
- `action_param_missing_type.awl` — untyped parameter → PARSE "parameter" @9.
- `call_site_retry_override.awl` — `retry` at a call site (only node/timeout may pin) → CHECK
  "retry" @15.
- `duplicate_action_name.awl` — same action twice on one worker → CHECK "duplicate" @10
  (inferred: "checked against the declaration" is ill-defined under duplicates).
- `action_unknown_return_type.awl` — return type never declared → CHECK "Ack" @10 (downstream
  untyped-binding cascades possible; substring pins the primary).
- `action_list_element_optional.awl` — `[String?]` action parameter (ruled 2026-07-11: `?`
  illegal in list-element position, signature surface) → CHECK "element" @9.

## step-bodies — calls, bindings, pipes, combinators, wait/sleep, literals

Valid:
- `step-bodies/valid/calls_and_side_effects.awl` — call with `->` binding, unbound side-effect
  call, zero-stage `binding |> route outcome` terminator.
- `step-bodies/valid/pipe_chain_stages.awl` — `value |> action |> .field -> name` chain split
  across steps.
- `step-bodies/valid/combinators.awl` — `filter`/`sort`/`map`/`count` with `.field` accessors,
  `is empty` guard.
- `step-bodies/valid/predicates_and_operators.awl` — all six comparisons, `not`/`and`/`or`,
  `is present`/`is absent` on `String?`.
- `step-bodies/valid/index_and_concat.awl` — literal-only indexing `xs[0]`, string `+`.
- `step-bodies/valid/wait_and_sleep.awl` — `wait signal -> name` (no timeout), `sleep 30s`.
- `step-bodies/valid/wait_timeout_optional.awl` — `wait … timeout 2d -> name` binding `T?`,
  `is present` guarded arm using the narrowed value (only fixture with the `d` duration unit).
- `step-bodies/valid/literal_forms.awl` — list literal `["a", "b"]`, float literal `0.5`,
  `true`, escaped string (`\"` `\n` `\t` `\\`), `///` doc on an action and on a step.
- `step-bodies/valid/step_bodies_combined.awl` — realistic multi-construct: pipe chains,
  combinators, indexing, `+`, sleep, wait+timeout, fall-through + route-targeted steps.

Invalid:
- `wait_missing_binding.awl` — `wait go` without `-> name` → PARSE "->" @15 (span on the wait
  statement line per source-correct-span discipline).
- `sleep_non_duration.awl` — `sleep 5` → PARSE "duration" @13 (judgment: grammar shows
  `sleep <duration>`).
- `computed_index.awl` — `responders[lead]` → PARSE "index" @14 (spec: literal-only, read as
  grammar).
- `equals_statement_binder.awl` — gone `=` statement binder → PARSE "=" @15.
- `pipe_missing_stage.awl` — `x |> -> name` → PARSE "|>" @15.
- `binding_missing_name.awl` — `->` with no name → PARSE "->" @15.
- `bare_field_statement.awl` — `.field` as a statement → PARSE ".greeting" @16.
- `pipe_chain_unterminated.awl` — chain without `-> name`/route → PARSE "pipe" @17 (judgment:
  "terminate with" read as grammar).
- `unknown_action.awl` — direct call of undeclared action → CHECK "polish" @16 (kept alongside
  declarations' pipe-call variant: different call surface).
- `unknown_binding.awl` — read of a never-bound name → CHECK "greeting" @18.
- `rebound_binding.awl` — single-assignment violation → CHECK "greeting" @16.
- `pipe_multi_arg_action.awl` — pipe stage action taking two args → CHECK "provision" @13.
- `unknown_field_access.awl` — `.volume` on a type without it → CHECK "volume" @19.
- `filter_non_bool.awl` — filter on a String accessor → CHECK "title" @17.
- `map_unknown_field.awl` — map projecting a missing field → CHECK "nope" @17.
- `count_with_argument.awl` — `count(.title)` → CHECK "count" @17 (judgment: arity is semantic).
- `is_present_non_optional.awl` — `is present` on plain String → CHECK "is present" @18.
- `is_empty_non_list.awl` — `is empty` on Int → CHECK "is empty" @19 (one fixture pins the
  rule; `is absent` shares it).
- `comparison_type_mismatch.awl` — Int < String → CHECK "<" @18.
- `arithmetic_plus.awl` — Int + Int (no arithmetic; `+` is string-only) → CHECK "+" @18.
- `and_non_bool.awl` — CHECK "and" @18.
- `not_non_bool.awl` — CHECK "not" @18.
- `wait_unknown_signal.awl` — wait on undeclared signal → CHECK "approval" @13.
- `pipe_route_unknown_outcome.awl` — pipe-route to unknown target → CHECK "finished" @19 (kept
  alongside loop-outcomes' outcome-clause variant: different surface).
- `pipe_route_payload_mismatch.awl` — piped Greeting into an outcome carrying Shouted → CHECK
  "Greeting" @15.
- `pipe_stage_type_mismatch.awl` — Int piped into a String-taking action → CHECK "Int" @18.
- `index_non_list.awl` — `topic[0]` on String → CHECK "index" @13.

## dag-fork — `after` graphs, fork/join

Valid:
- `dag-fork/valid/after_single.awl` — one explicit `after` edge.
- `dag-fork/valid/after_multi_diamond.awl` — diamond: two parallel steps sharing `after gather`,
  `after claims, citations` join.
- `dag-fork/valid/fall_through_chain.awl` — pure fall-through, no after/route until final pipe.
- `dag-fork/valid/fork_action_fanout.awl` — parallel action collection fork
  (the doc-certification shape): one unbound action call with a captured free
  name, `join -> name` feeding the route payload directly (no combinators —
  the BC-2b-4 direct-lowering flagship, BEAM-golden'd).
- `dag-fork/valid/fork_collection_join.awl` — `fork x in xs … join -> name`,
  count/filter/is-empty discrimination.
- `dag-fork/valid/fork_named_homogeneous.awl` — bare `fork`, two branches of
  the SAME action → one typed `workflow.all`, source-order destructure.
- `dag-fork/valid/fork_sequential_route.awl` — `sequential` collection fork
  whose joined list routes directly (no combinators — direct-lowerable).
- `dag-fork/valid/fork_named_branches.awl` — bare `fork` heterogeneous branches with per-branch
  bindings, bare `join` (no `->`), branch bindings consumed downstream.
- `dag-fork/valid/fork_sequential.awl` — `fork x in xs sequential … join -> name`.
- `dag-fork/valid/release_pipeline_combined.awl` — fall-through root, diamond, collection fork,
  sequential fork, route-targeted step, per-action node/timeout/retry config.

Invalid (all CHECK — the family's required errors are semantic):
- `unknown_after_target.awl` — CHECK "missing_step" @16 (span on the step header).
- `unknown_after_second_target.awl` — unknown name in SECOND `after` position → CHECK
  "phantom" @16 (span-discipline case).
- `after_self_cycle.awl` — `step settle after settle` → CHECK "cycle" @11.
- `after_cycle_pair.awl` — mutual after edges → CHECK "cycle" @11 (first-written participant;
  fixture asserts the cycle diagnosis wins over any unreachable-step diagnosis).
- `route_cycle_self_unbounded.awl` — otherwise arm routes to its own step, no bound → CHECK
  "bound" @17 (span on the routing outcome line).
- `route_cycle_two_steps_unbounded.awl` — pure route-route cycle, no bound → CHECK "bound" @24
  (backward-edge route line).
- `unreachable_step.awl` — step no route targets, below a step that always pipe-routes →
  CHECK "orphan" @15.
- `fork_non_list_collection.awl` — `fork item in doc` over a bare record →
  CHECK "needs a list" @13.

Note: dag-fork route-cycle sidecars pin "bound", loop-outcomes' mixed-cycle pins "cycle" — the
diagnostic wording must carry both (e.g. "route cycle without a bound").

## loop-outcomes — loop/counting, outcome arms, routing, on failure, substeps

Valid:
- `loop-outcomes/valid/loop_counting_until_max.awl` — `loop x = seed counting n … until … max`.
- `loop-outcomes/valid/loop_without_counting.awl` — bounded `loop x = seed … until … max`
  without a public counter (pins the scalar loop result and no-destructure call site).
- `loop-outcomes/valid/loop_compound_until_nested.awl` — nested bounded loops with `and` and
  optional-narrowing `or` compound `until` conditions (pins short-circuit ordering and nested
  loop-slot planning).
- `loop-outcomes/valid/backward_route_bounded_cycle.awl` — backward route legal because a
  max-bounded loop sits on a step in the cycle; counter used in a later step's payload.
- `loop-outcomes/valid/loop_after_fall_through.awl` — a fall-through chain whose terminal
  step loops on the first step's binding (pins the interior chain boundary carrying the
  loop's seed and ceiling names).
- `loop-outcomes/valid/route_outcome_by_name.awl` — bare `route <outcome>` picks up the
  in-scope binding NAMED like the outcome (interpretation: by-name, not by-type; the negative
  is `bare_route_no_binding`).
- `loop-outcomes/valid/enum_when_totality.awl` — enum-subject `when x.category == Variant`
  arms, total without `otherwise` (pins bare-TitleCase variant comparison syntax for declared
  enums; imported string enums compare against string literals, see import_nested_defs).
- `loop-outcomes/valid/guard_optional_wait.awl` — wait+timeout `T?` used as `T` inside the
  `is present` arm (kept alongside wait_timeout_optional: Nil-returning config'd action, 30m).
- `loop-outcomes/valid/on_failure_compensation.awl` — `on failure` calls then
  route-to-workflow-outcome.
- `loop-outcomes/valid/substeps_two_stage.awl` — substep grammar: inner route to a sibling
  substep and to a parent outcome clause (interpretation: routing to the parent's outcome
  NAME fires that outcome's route; its payload uses literals only).
- `loop-outcomes/valid/ship_release_combined.awl` — loop+counting, wait guard, backward route,
  `on failure` ending route-to-step, counter reused downstream.

Invalid:
- `loop_missing_seed.awl` — loop without `= seed` → PARSE "=" @18.
- `counting_missing_name.awl` — `counting` without a name → PARSE "counting" @18.
- `counting_shadows_loop_binding.awl` — public counter reuses the threaded binding name →
  CHECK "counting" @13 (the values have distinct types and Gleam forbids a duplicate tuple binder).
- `outcome_missing_route.awl` — `when` arm with no route → PARSE "route" @23.
- `loop_no_max.awl` — unbounded loop → CHECK "unbounded" @15 (judgment: legality rule, not
  grammar; reclassify PARSE if `max` becomes grammar-mandatory).
- `loop_no_rebind.awl` — body never rebinds the threaded value → CHECK "rebind" @18 (anchored
  on the loop line).
- `loop_exhaustion_uncovered.awl` — ceiling case uncovered → CHECK "exhaust" @15 (anchored on
  the step header, match-expression convention).
- `when_without_otherwise.awl` — lone when, non-exhaustive → CHECK "exhaust" @13.
- `enum_variant_uncovered.awl` — Spam lane missing → CHECK "Spam" @15 (diagnostic must NAME
  the missing variant).
- `otherwise_not_last.awl` — CHECK "otherwise" @23 (anchored on the misplaced otherwise).
- `route_unknown_target.awl` — outcome-clause route to unknown target → CHECK "report" @23.
- `route_cycle_unbounded.awl` — mixed fall-through+route cycle, no bound → CHECK "cycle" @24.
- `on_failure_no_route.awl` — compensation without terminal route → CHECK "route" @26 (span on
  the `on failure` line).
- `optional_used_unguarded.awl` — `T?` read in an unguarded otherwise arm → CHECK "reply" @22
  (canonical for the flow-typing rule; step-bodies duplicate removed).
- `payload_missing_field.awl` — payload omits a required field → CHECK "polls" @23 (canonical;
  schema-doors duplicate removed).
- `bare_route_no_binding.awl` — bare `route summary` with no binding named `summary` in scope
  → CHECK "summary" @18 (flips to valid if the pickup rule is by-type).
- `final_step_falls_off.awl` — final step never routes → CHECK "route" @13 (span on the step
  header).
- `substep_route_escape.awl` — inner route targets a step outside the parent → CHECK
  "parent" @24.
- `binding_not_on_all_paths.awl` — step reachable by two routes reads a binding made on only
  one of them (spec: "the checker rejects reads of bindings not guaranteed on every path into
  a step") → CHECK "trimmed" @31.
- `loop_step_no_outcomes.awl` — loop-carrying step with ZERO outcome clauses (ruled 2026-07-11:
  exhaustion must be explicitly named; the strong reading) → CHECK "exhaust" @16 (anchored on
  the loop line — the sibling of `loop_exhaustion_uncovered`'s step-header anchor).
- `route_in_loop_body.awl` — `route` statement inside a loop body (ruled 2026-07-11: loops exit
  via `until`/`max`; routing is the step's outcomes' job) → CHECK "`loop` body" @17 (span on
  the route).
- `pipe_route_in_loop_body.awl` — pipe-chain `|> route` terminator inside a loop body (same
  ruling, pipe surface) → CHECK "`loop` body" @17 (span on the route target).

## Corpus-wide conventions and recorded judgments

- Every valid fixture is a complete workflow: `//!` narration, header with >=1 outcome, worker
  actions for every call, reachable terminal routes; <=100 columns; two-space indents;
  newline-terminated; no tabs.
- Every invalid fixture carries exactly one defect. Substrings are offender identifiers,
  keywords, or operators where possible (wording-agnostic); a few force phrasing ("named",
  "duration", "pipe", "unbounded", "exhaust", "parent") and are contract-setting.
- Duplicate-declaration spans anchor at the second occurrence. Imported-schema errors anchor at
  the `type X = schema("…")` declaration line; inline-schema errors at the offending JSON line.
- Stage assignments where the spec names the error but not the stage are recorded per-fixture
  above; sidecar line-1 values ARE the family ruling to revisit, not settled spec text.
- Unused declarations (types/signals/inputs/actions/bindings) are treated as legal — the spec's
  checker duty list has no unused-decl rule. Unreachable workflow OUTCOMES are not an error
  (the checker list covers unreachable steps only).
- Deliberately NOT fixtured (spec-silent; no grammar invented): enum totality via anything but
  `== Variant`; compound guards (`when x is present and …`); combinator literal arguments;
  `sort` key comparability; parenthesized boolean grouping; empty list literal `[]`;
  `T??`; trailing `|` in enums; zero-arg actions; declaration-order violations beyond the two
  covered; `join -> name` after named branches / bare `join` after collection forks;
  `required` naming an absent property; top-level non-object imported schema; explicit null in
  input documents (start-time, not statically expressible); out-of-range literal index
  (a runtime step failure per spec); config-key ordering/duplication on config lines;
  unconditional bare `outcome name:` clauses (grammar shows only when/otherwise);
  gone-list keywords beyond the four representatives (`about`, `Option`, `List`, `=`-binder,
  plus `finish`) — one class, five witnesses.
- Type-brace column alignment — RULED at the parser/printer phase (2026-07-10): group
  alignment IS canonical. Within a maximal run of adjacent single-line type declarations of
  the same form (`{ … }` bodies, or `= …` doors/enums; runs break on blank lines, comments,
  doc lines, or a multi-line declaration), names pad to a common column; workflow-header
  `outcome` runs align their `type` and `route` columns the same way. Alignment padding is
  exempt from the 100-column rule (the single-line decision is made unpadded). All valid
  fixtures were mechanically re-normalized through the printer to match; the flagship pair
  needed (and received) no edits — `print(parse(dev_brief.awl))` is byte-identical.
  (noncanonical_commas remains excluded from byte-identity goldens.)
- Checker-phase rulings (2026-07-11), recorded where the sidecars left the stage or rule open:
  every CHECK-staged sidecar passed at its recorded stage — no re-staging was needed (the
  `construct_null_payload` judgment note resolves to CHECK: `null` lexes as an identifier and
  the checker refuses the reference everywhere with a targeted absence-is-omission diagnostic).
  Bare `route <outcome>` pickup is BY NAME, as `bare_route_no_binding` anticipated. Optional
  (`?`) parameters may be omitted at call sites, mirroring record construction (spec-silent;
  one rule for both argument surfaces). Type compatibility is structural for record shapes so
  a schema-projected record satisfies a declared record with the same fields (dev_brief's
  `config.lenses` items vs the declared `Lens`); display names stay nominal in diagnostics.
  Route-cycle boundedness accepts a `max`-bounded loop on any step of the control-flow SCC
  (route edges + fall-through edges + `after` edges — CORRECTED at the checker fix round,
  2026-07-11: a dependency's completion re-arms its dependents, so a backward route plus a
  forward `after` edge is as unbounded a cycle as two routes; the earlier narrower SCC
  contradicted the spec's "unbounded cycles are unwritable"); the diagnostic anchors on the
  first backward or self route edge (falling back to any in-cycle route edge, then the
  earliest member's header) and its wording carries both pinned substrings ("cycle",
  "bound"). The route-cycle/exhaustiveness/`unbounded` trio all anchor exactly where the
  sidecars pinned: backward-edge route line / step header / loop header line respectively.
- Outcome-clause layout — RULED at the same phase: a payload-constructing route
  (`route out(field: …)`) ALWAYS breaks after the guard comma onto its own line one level
  deeper; a bare route stays on the guard line when the clause fits 100 columns. The spec's
  printer-contract prose ("payload construction breaks … when over 100 columns") and its
  worked examples (which break 90- and 99-column payload clauses) disagree; byte-identity
  with the flagship pins the examples' reading. Valid fixtures were re-normalized to match
  (splits/joins of outcome clauses only — audited via whitespace-insensitive diff).
- Checker fix-round rulings (2026-07-11 adversarial panel; regression suites
  `checker_regressions.rs` + `checker_hardening.rs` pin each one): named-branch fork branches
  walk in isolated clones of the pre-fork scope and merge bindings at `join` (a sibling's
  binding is unreadable mid-fork); `join -> name` on the named form is refused (spec shows
  bindless `join`). Every non-terminal falling-through step must have its completion consumed
  (the next step's fall-through edge or an `after` dependent) — the successor duty is no
  longer final-step-only. A piped route must target a workflow outcome; step, sibling, and
  parent-arm targets are refused (silent value loss). Steps and workflow outcomes share one
  route-target namespace (collision is a declaration-time error anchored at the step). A bind
  inside a collection-fork branch is NOT a loop rebind (branch bindings never escape).
  Inline schema-door diagnostics anchor by walking the raw JSON to the failing path, never by
  first token occurrence. Structural compatibility carries no acceptance depth cap
  (coinductive in-progress pair set; recursive types still terminate). Dead control flow is
  refused: statements behind an unconditional body route and outcome clauses behind a
  body-terminal route. Call-site config on CHILD calls is refused (the engine routes
  children, not a queue).
- Ratified rulings (Tom, 2026-07-11) — the two items previously OPEN here are RESOLVED, plus a
  third moved up from the emitter stopgap:
  - R1, loop exhaustion must be explicitly named: a step whose body contains a `loop` (fork
    branches included; a substep answers for its own clauses) MUST declare conditional outcome
    clauses covering the exhausted case — a loop-carrying step with zero outcome clauses is a
    CHECK error anchored on the loop (`loop_step_no_outcomes`). The permissive fall-through
    reading is dead.
  - R2, `?` is illegal in list-element position: `[T?]` is refused at CHECK in every type
    position (shorthand fields, inputs/signals/outcomes, action/child signatures — one rule at
    type-reference resolution), `[T]?` stays legal, and the imported-schema projection cannot
    manufacture the shape (only object properties wrap in `?`; a null-admitting `items` type
    is already refused as a null union). Fixtures: `list_element_optional`,
    `action_list_element_optional`; the remaining positions are pinned by
    `tests/checker_rulings.rs`.
  - R3, `route` is illegal inside a `loop` body: statement and pipe-chain `|> route`
    terminator alike, refused at CHECK with the span on the route (`route_in_loop_body`,
    `pipe_route_in_loop_body`). The emitter's emit-time refusal survives only as a defensive
    backstop for unchecked documents. No sidecar re-staging was needed: every sidecar stage
    was already PARSE/CHECK — the old route-in-loop refusal was pinned by an emitter unit
    test, now reframed as the backstop pin.
