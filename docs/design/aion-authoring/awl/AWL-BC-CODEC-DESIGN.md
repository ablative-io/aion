# AWL-BC-0 — descriptor-driven codec engine and the aion_flow hoist

Status: DESIGN for BC-0 implementation. Parent: `AWL-BC-BUILD-PLAN.md`
(locked decision D-BC2, recon deltas 5/6) and `AWL-BC-DESIGN-DRAFT.md` §2.
Authored 2026-07-11 on the `awl-bc0` worktree.

**Decision: `descriptor-full`** — descriptors drive BOTH encode and decode,
with a single identity-coerce boundary inside the SDK engine, implemented by
a tiny Erlang shim source in `aion_flow` (precedent: the package already
ships `src/aion_flow_query_pump.erl`). The pre-authorized fallback
(hoist-only) remains live if the safety argument in §4 fails review; the
encode-only middle option is evaluated and rejected in §9.

The rest of this document specifies: the descriptor value (§2), the engine
(§3), the coerce boundary and its safety argument (§4), the D4/wire-parity
contract (§5), the fixed-glue hoist and module map (§6), the emitter changes
(§7), the test plan (§8), the options evaluation (§9), and risks/open
questions (§10–11).

## 1. What problem this solves

Recon delta 5: ~45% of a rev-2 generated module is workflow-independent.
The per-type codec trios (`x_codec` / `x_to_json` / `x_decoder`, ~105 lines
in `awl_hello`) are a pure function of the emitter's `TypeEnv` — structurally
uniform, semantically identical across modules. D-BC2 moves them out of the
emitted surface entirely: the module carries **type descriptors as plain
literals** and the SDK owns ONE codec engine that interprets them. This is
the single biggest reduction of BC-3's instruction-selection surface, and it
collapses D4 optionality (absent-vs-null) to one canonical implementation.

The hard problem, stated up front (it shapes the whole design): a generic
DECODE in Gleam produces `Dynamic`, but the generated module constructs and
destructures **nominal** record types (`Greeting`, `AwlHelloInput`, …).
Gleam source has no safe cast from `Dynamic` to a nominal type. §3–§4 are
the honest treatment; §9 records why we still choose full descriptors.

## 2. The descriptor value

D-AOT2 constraint: the descriptor is a **plain data value** — expressible as
Gleam module constants today and as ETF `LitT` literals in BC-2/BC-3. No
closures, no function references, nothing but constructors of a closed ADT
over strings, ints, lists, and tuples.

New module `aion/awl/descriptor` in `aion_flow`:

```gleam
/// The wire-type descriptor set. Mirrors the emitter's `GType` exactly
/// (crates/aion-awl/src/emitter/types.rs); one constructor per wire shape
/// the checker can put on a boundary.
pub type Desc {
  DBool
  DInt
  DFloat
  DString
  /// Encodes as `{}`; decodes accepting anything (parity with today's
  /// `nil_codec`). Also the lowering for `GType::Unknown`.
  DNil
  DList(Desc)
  /// A whole-value option (non-field position): today's nullable form —
  /// `json.nullable` on encode, `decode.optional` on decode.
  DNullable(Desc)
  /// Reference into the defs table. Keeps descriptors finite under
  /// recursive named types and dedups shared records.
  DRef(String)
}

/// One record field. `optional: True` is D4 field optionality: the Gleam
/// value is `Option(inner)`; encode omits the pair when `None`; decode uses
/// `decode.optional_field` (absent → None, explicit null → decode FAILURE).
pub type Field {
  Field(name: String, desc: Desc, optional: Bool)
}

/// A named definition in the defs table.
pub type Def {
  /// `tag` is the record's Erlang constructor atom text (pre-snaked by the
  /// emitter). Zero fields ⇒ the runtime value is the bare atom.
  DRecord(tag: String, fields: List(Field))
  /// Payload-less enum: encodes as the variant's PascalCase name string
  /// (today's behavior); each entry is #(json_name, atom_text).
  DEnum(name: String, variants: List(#(String, String)))
  /// The outcome union: `{"outcome": name, "payload": …}`. Each arm is
  /// #(outcome_name, constructor_atom_text, payload_desc). `name` feeds
  /// `decode.failure`'s expected-name (parity with today).
  DUnion(name: String, arms: List(#(String, String, Desc)))
}

/// The per-module registry, mirroring `TypeEnv.defs` (aliases resolved away
/// at emission; only records, enums, and unions appear).
pub type Defs =
  List(#(String, Def))
```

Notes pinned by the current emitter surface:

- **`GType::Duration` never reaches a wire position.** No `duration_ms_codec`
  is emitted anywhere today (durations are call-site config literals,
  `duration.milliseconds(n)`); the descriptor set therefore has no duration
  leaf. If the checker ever admits one, the engine refuses at `from` with an
  explicit error rather than guessing a representation.
- **Aliases are resolved at emission** (`TypeEnv::resolve`), exactly as
  `codec_name` does today; `DRef` targets are always records/enums/unions.
- **Enums carry both spellings**: the JSON string is the PascalCase variant
  name (today: `json.string("Shouted")`-style), the runtime value is the
  snake_case atom the Gleam compiler assigns the constructor.
- **All atom texts are pre-computed by the emitter** (which already owns
  `names::snake`); the engine never derives atom text from a JSON name.
- Every constructor above is atoms + binaries + lists + small tuples at the
  BEAM level — directly expressible as an ETF `LitT` literal in BC-2/BC-3
  and as a `const` in Gleam source now.

The generated module carries one `const awl_defs: descriptor.Defs = [...]`
plus inline root descriptors at each boundary call site (usually a bare
`DRef("Greeting")` or a leaf).

## 3. The engine (`aion/awl/codec`)

One public entry point:

```gleam
/// Build a typed codec from a descriptor.
///
/// CONTRACT (unsafe if violated): `root`/`defs` must describe the runtime
/// representation of `a`. Generated AWL modules satisfy this by
/// construction: the same TypeEnv that declared `a` emitted the descriptor.
/// This function exists for generated code; hand-written workflows should
/// use `aion/codec.json_codec` with real decoders.
pub fn from(root: descriptor.Desc, defs: descriptor.Defs) -> codec.Codec(a)
```

The centerpiece design move: **the engine reuses `gleam/dynamic/decode` and
`aion/codec.json_codec` rather than hand-rolling a JSON walk.** `from` is
literally:

```gleam
pub fn from(root, defs) -> codec.Codec(a) {
  codec.json_codec(
    fn(value) { encode_walk(reflect.to_dynamic(value), root, defs) },
    decode.map(build_decoder(root, defs), reflect.from_dynamic),
  )
}
```

Because the composed decoder flows through the SAME `codec.json_codec`
error-mapping (`json_decode_error` / `dynamic_decode_error` in
`aion/codec.gleam`), decode-failure `reason`/`path` strings are structurally
identical to today's generated decoders — the failure-path parity concern
(those reasons feed `AwlDecodeInputFailed` messages, which are trail-visible)
dissolves by construction.

### 3.1 Encode: walk a typed value as Dynamic

`encode_walk(value: Dynamic, desc: Desc, defs: Defs) -> json.Json`.
Encode consumes values the typed module constructed, so every step is
shape-guaranteed by the descriptor's provenance:

- Leaves: coerce and delegate — `json.string(reflect.coerce(value))`, same
  for int/float/bool; `DNil` → `json.object([])`.
- `DList(inner)`: coerce to `List(Dynamic)` (a list of `a` IS a list of
  Dynamic at the BEAM level), `json.array(_, encode_walk(_, inner, defs))`.
- `DNullable(inner)`: coerce to `Option(Dynamic)` (representation-safe:
  `none` / `{some, V}`), then `json.nullable(_, encode_walk(_, inner, defs))`
  — today's whole-value form exactly.
- `DRef(name)` → look up the def:
  - `DRecord(tag, fields)`: `reflect.record_fields(value)` returns the field
    list (`[]` for the bare-atom zero-field case); zip with `fields`; build
    `json.object(list.flatten([...]))` where a required field contributes
    `[#(name, encoded)]` and an optional field contributes `[#(name,
    encoded_inner)]` for `Some(inner)` and `[]` for `None` — byte-for-byte
    today's D4 encode, including field order (descriptor order = today's
    emission order) and the `list.flatten` object shape.
  - `DEnum(_, variants)`: `reflect.atom_text(value)`, find the matching
    atom_text, emit `json.string(json_name)`.
  - `DUnion(_, arms)`: `reflect.record_tag(value)` (the constructor atom),
    find the arm, emit `json.object([#("outcome", json.string(name)),
    #("payload", encode_walk(payload, payload_desc, defs))])` where payload
    = the single constructor argument.

A value/descriptor mismatch (impossible for generated modules, possible for
a hand-mis-used `from`) fails loudly — `json.string` on a non-binary is a
badarg crash, and `record_fields`/lookup misses surface as an explicit
`panic`-free refusal (see §3.3) — never a silent wrong encoding.

### 3.2 Decode: compose a `decode.Decoder(Dynamic)` from the descriptor

`build_decoder(desc: Desc, defs: Defs) -> decode.Decoder(Dynamic)`:

- Leaves: `decode.string |> decode.map(reflect.to_dynamic)` (and int/float/
  bool). `DNil`: `decode.success(reflect.to_dynamic(Nil))` — accepts
  anything, exactly like today's `nil_decoder`.
- `DList(inner)`: `decode.list(build_decoder(inner, defs)) |> decode.map(to_dynamic)`.
- `DNullable(inner)`: `decode.optional(build_decoder(inner, defs))
  |> decode.map(to_dynamic)` (the Option(Dynamic) IS the Option(a)
  representation).
- `DRecord(tag, fields)`: a right fold producing the same `use`-chain shape
  the generated code has today:
  - required: `decode.field(name, inner_decoder, continue)`
  - optional: `decode.optional_field(name, to_dynamic(option.None),
    decode.map(inner_decoder, wrap_some), continue)` — absent → `None`;
    **present-`null` runs the inner decoder on `null` and FAILS** — D4
    exactly, same primitive as today so the same failure text.
  - terminal: `decode.success(reflect.make_record(tag, collected))` where
    `make_record` builds `{tag_atom, F1, …, Fn}` — or the bare atom when the
    field list is empty (zero-arg constructors are atoms, not 1-tuples).
- `DEnum(name, variants)`: `use s <- decode.then(decode.string)`; match the
  json_name → `decode.success(reflect.make_record(atom_text, []))`; fallback
  `decode.failure(to_dynamic(Nil), name)` — the default value never escapes
  a failure, so replacing today's typed zero value with a Dynamic Nil is
  observationally invisible; the expected-name string is preserved.
- `DUnion(name, arms)`: `use outcome <- decode.field("outcome",
  decode.string)`; matching arm → `use payload <- decode.field("payload",
  payload_decoder)` → `decode.success(make_record(tag, [payload]))`;
  fallback `decode.failure(to_dynamic(Nil), name)`.
- `DRef(name)`: **lazily** — `decode.then(decode.dynamic, fn(_) {
  build_ref_decoder(name, defs) })` so recursive named types (legal today
  via optional/list fields; only *required*-field recursion is refused, by
  `zero_expr`) terminate at construction time. If the pinned stdlib provides
  `decode.recursive`, use it instead; the `then` form is the guaranteed
  fallback on the current dependency floor.

A `DRef` miss (descriptor bug) resolves to a decoder that always fails with
an explicit `decode.failure(to_dynamic(Nil), "unknown type " <> name)` —
surfaced, never silent.

### 3.3 Union-decoder default values: a deliberate non-change

Today the union decoder's failure arm needs a typed zero value
(`TypeEnv::zero_expr`), and the emitter REFUSES documents whose first
success outcome recurses through required fields. The engine no longer
needs zero values at all (Dynamic Nil suffices). **BC-0 keeps the emitter
refusal anyway** (parity-first, D-BC3 spirit: a refactor must not widen the
accepted surface); removing it is ticketed as a post-BC capability change.

## 4. The coerce boundary and its safety argument

### 4.1 The shim

New Erlang source `gleam/aion_flow/src/aion_flow_awl_ffi.erl` (compiled by
`gleam build` alongside the package, exactly like `aion_flow_query_pump.erl`
today), fronted by `aion/awl/internal/reflect.gleam` — the ONLY module that
declares these externals:

```erlang
%% Identity at the BEAM level. Both coercion directions in the codec
%% engine bottom out here; see the safety argument in
%% docs/design/aion-authoring/awl/AWL-BC-CODEC-DESIGN.md §4.
identity(X) -> X.

%% {tag, F1..Fn} construction/destructuring, honoring Gleam's zero-arg
%% constructor = bare atom representation.
make_record(TagBin, [])     -> binary_to_atom(TagBin, utf8);
make_record(TagBin, Fields) -> list_to_tuple([binary_to_atom(TagBin, utf8) | Fields]).
record_fields(V) when is_atom(V)  -> [];
record_fields(V) when is_tuple(V) -> tl(tuple_to_list(V)).
record_tag(V) when is_atom(V)  -> atom_to_binary(V, utf8);
record_tag(V) when is_tuple(V) -> atom_to_binary(element(1, V), utf8).
```

`reflect.gleam` types them:

```gleam
@external(erlang, "aion_flow_awl_ffi", "identity")
pub fn to_dynamic(value: a) -> Dynamic          // safe direction, always

@external(erlang, "aion_flow_awl_ffi", "identity")
pub fn from_dynamic(value: Dynamic) -> a        // THE unsafe direction

@external(erlang, "aion_flow_awl_ffi", "identity")
pub fn coerce(value: Dynamic) -> b              // leaf reads inside encode_walk
// + make_record / record_fields / record_tag / atom_text as above
```

`binary_to_atom` (not `binary_to_existing_atom`) is deliberate: a record
that only ever crosses the codec boundary (never constructed or tag-matched
in module code — field access compiles to `element/2` and references no
atom) may not have its tag atom in the atom table when the first decode
runs. Atom creation is bounded by descriptor content, which is bounded by
the checked document — no unbounded-atom risk.

### 4.2 The safety argument (written down, as required)

The one genuinely unsafe operation is `from_dynamic`: telling Gleam that a
decoder-built `Dynamic` is a value of nominal type `a`. Why this is sound
for generated modules:

1. **Single provenance.** The descriptor and the nominal type declarations
   are emitted by the same emitter from the same `TypeEnv`, in the same
   generated module, in the same compilation unit. There is no version skew
   axis: descriptor and type cannot drift independently within a module.
2. **Representation identity.** Gleam's Erlang representation is fixed and
   documented (draft §4 ABI table): record = `{snake_tag, F1..Fn}`, zero-arg
   constructor = atom, `Option` = `{some,V}`/`none`, lists/binaries/numbers
   structural. `build_decoder` constructs exactly these shapes, field-for-
   field in declaration order. At the BEAM level the decoded term **is** the
   record; the coerce changes only the type checker's opinion, not one bit
   of the term.
3. **Type-checker anchoring at the call site.** `from(root, defs)` returns
   `Codec(a)` with `a` bound by the surrounding typed context
   (`workflow.define(name, input_codec, output_codec, error_codec, execute)`
   ties all three codecs to `execute`'s signature). The generated module
   cannot accidentally use the wrong `a`; only the wrong *descriptor* — and
   the descriptor is generated from the same source of truth as `a`.
4. **Triple-tested.** (a) SDK unit tests pin every constructor shape against
   hand-written expected terms; (b) each regenerated example/fixture gets a
   round-trip contract test (construct typed value with the real generated
   constructors → encode → decode → assert `==`), which catches any
   tag-snaking drift against the actual Gleam compiler; (c) the BC-4
   differential trails oracle covers it end-to-end forever.
5. **Contained blast radius.** `from_dynamic` is used at exactly one point
   (the `decode.map` in `from`) inside one internal module; `from` itself is
   the only public consumer. The doc comment states the contract; the
   `aion/awl/*` namespace is documented as reserved for generated code. A
   violated contract produces either an explicit decode failure or a crash
   in the workflow process (function_clause / badarg on first typed use) —
   loud, never a silently wrong value *of the right type*, because a term of
   the wrong shape cannot satisfy later typed destructuring.

What this argument does NOT cover — and why that is acceptable: a
hand-written Gleam workflow calling `from` with a lying descriptor. That is
the same class of self-inflicted damage as writing `@external` yourself; the
SDK cannot prevent it, only document it (point 5) and keep `json_codec` as
the front door for hand-written code.

## 5. Wire-parity contract (D4 and friends)

The engine must be observably identical to today's generated codecs. The
binding behaviors, each with a dedicated SDK test:

| Behavior | Today (generated) | Engine |
|---|---|---|
| Optional field, value `None` | pair omitted (`list.flatten` arm `[]`) | identical, same mechanism |
| Optional field, absent on decode | `decode.optional_field` → `None` | identical primitive |
| Optional field, explicit `null` on decode | inner decoder runs on null → **failure** | identical primitive |
| Whole-value option (non-field) | `json.nullable` / `decode.optional` | `DNullable`, same primitives |
| Record field order | declaration order in `json.object` | descriptor order = declaration order |
| Zero-field record | bare atom; encodes `{}` via field-less object | `make_record(tag, [])` → atom; `json.object([])` |
| Enum | `json.string(VariantName)`; unknown → `decode.failure(first, Name)` | same strings; failure default Dynamic-Nil (invisible), same expected-name |
| Union | `{"outcome": …, "payload": …}`; unknown → `decode.failure(zero, UnionName)` | identical shape; failure default invisible |
| `Nil` | encodes `{}`; decoder accepts anything | `DNil` identical |
| Decode error text | `codec.json_codec` mapping of `gleam/dynamic/decode` errors | **same code path** (§3) |
| Encode bytes | `json.to_string` insertion order | identical (same construction order) |

Gate (from the build plan): goldens re-baselined deliberately; the
committed `awl_hello` e2e trail unchanged; compile proofs green;
generated-module line count drops ≥40% (measured: `awl_hello` 344 → ~140
lines, ≈59%).

## 6. The fixed-glue hoist: module map

All new modules live in `gleam/aion_flow` (version bump 0.5.0 → 0.6.0;
additive, no existing public API changes):

| New module | Hosts | Replaces (per-module generated code) |
|---|---|---|
| `aion/awl/error.gleam` | `pub type AwlError` (all 9 variants, unchanged shape/atoms); `pub fn codec() -> Codec(AwlError)` (hand-written, NOT descriptor-driven — it is fixed glue with a heterogeneous 2-field variant; literal port of today's `awl_error_*` trio); the 5 mappers `map_activity_error`, `map_receive_error`, `map_child_error`, `map_spawn_error`, `map_timer_error` | `pub type AwlError` + `awl_error_codec/_to_json/_decoder` (~70 lines) + 5 mappers (~20 lines) |
| `aion/awl/descriptor.gleam` | `Desc`, `Field`, `Def`, `Defs` (§2) | — (new) |
| `aion/awl/codec.gleam` | `from(root, defs)` (§3); `raw() -> Codec(String)` (today's `awl_raw_codec`); `decoded(codec, payload, action) -> Result(a, AwlError)` (today's `awl_decoded`); `json_value() -> Codec(json.Json)` (encode-only child-input codec, refusal message preserved verbatim) | per-type codec trios (~105 lines in awl_hello), 15 builtin leaf fns, composite list/option trios, flag-gated raw/decoded/json_value helpers |
| `aion/awl/runtime.gleam` | `run(raw_input: Dynamic, input_codec, output_codec, execute) -> Result(String, AwlError)` — the generic run shell (decided: **hoist**, §6.1); `index(items, index, label) -> Result(a, AwlError)` (today's `awl_index`) | the `run()` body (~15 lines) + `awl_index` (~7 lines) |
| `aion/awl/internal/reflect.gleam` | the `@external` declarations (§4.1), internal-only by convention | — (new) |
| `src/aion_flow_awl_ffi.erl` | identity + record reflection, safety comments | — (new) |

**`try` is NOT hoisted — it is deleted.** The generated `try` is exactly
`gleam/stdlib`'s `result.try`; call sites become
`use x <- result.try(...)` with `import gleam/result`. Zero SDK code, same
semantics, one less name.

Note on `AwlError`'s move: variants compile to the same global atoms and
tuple shapes regardless of which module declares the type, and the codec
emits the same JSON — nothing durable or wire-visible records the declaring
module. Generated code references it qualified (`awl_error.AwlError`); no
compatibility alias is emitted (nothing outside the module names the old
path — the engine invokes `run/1` structurally).

### 6.1 The generic `run()` shell — decided: hoist

`runtime.run` takes the raw Dynamic, both codecs, and `execute`, and
reproduces today's three-stage case tree with the exact error strings
(`"workflow input payload was not a string"`, `"failed to decode workflow
input: " <> reason`). The generated module keeps a 3-line exported wrapper —
required, because the engine invokes the module's own `run/1` by name:

```gleam
pub fn run(raw_input: Dynamic) -> Result(String, awl_error.AwlError) {
  runtime.run(raw_input, input_codec(), outcome_codec(), execute)
}
```

This is a closure-free, name-substituted shell — exactly the MIR template
shape BC-2/BC-3 want.

### 6.2 What stays generated (name-substituted glue, by design)

`definition()`, the `run` wrapper, `execute` + step functions + loop
functions, activity wrappers (task-queue baked in) and their raw twins,
signal refs, the type declarations, the descriptor constants, and the retry
loop (emitted per `retry` use in `loops.rs`). The retry loop is
deliberately OUT of BC-0 scope: it is not in the recon delta-5 fixed block
nor the task list; it captures per-site config and a body closure and is
already a clean MIR template for BC-2/BC-3. Ticketed as a possible later
SDK combinator, not blocking.

## 7. Emitter changes (`crates/aion-awl/src/emitter`)

1. **`codecs.rs` + `composites.rs` → `descriptors.rs`.** Delete the error
   codec, record/enum/union trio generators, composite trios, and builtin
   leaf trios. New code renders (a) `const awl_defs: descriptor.Defs = [...]`
   from `TypeEnv` (records with pre-snaked tags and D4 optionality on
   `Field`, enums with `#(json_name, atom_text)` pairs, the outcome union as
   a `DUnion` def) and (b) a `GType → Desc` expression renderer replacing
   `codec_name` at call sites (leaves inline, named types as `DRef`,
   aliases resolved first). One generated 1-line helper
   `fn awl_codec(root) { awlc.from(root, awl_defs) }` keeps call sites
   short.
2. **`frame.rs`.** `error_type()` deleted; header imports gain
   `aion/awl/codec as awlc`, `aion/awl/descriptor`, `aion/awl/error as
   awl_error`, `aion/awl/runtime`, `gleam/result` and drop now-unused ones
   (`gleam/json` stays only if still referenced, e.g. child-input assembly);
   `definition()` binds `awl_error.codec()`; `run()` becomes the §6.1
   wrapper.
3. **`wrappers.rs`.** `helpers()` shrinks to nothing (index → `runtime.index`,
   raw/decoded/json_value → `awlc.*`, mappers → `awl_error.*`); activity
   wrappers/signal refs switch codec references to `awl_codec(...)` calls.
4. **Call sites** (`steps.rs`, `pipes.rs`, `outcomes.rs`, `loops.rs`,
   `forks.rs`, `stmts.rs`): `try(` → `result.try(`, mapper names →
   `awl_error.map_*`, `awl_index` → `runtime.index`.
5. **`types.rs`.** `zero_expr` retained (§3.3). `codec_name` retires in
   favor of the descriptor renderer (keep the stem logic only if goldens
   still need stable helper naming).
6. **Flags** (`context.rs`): `raw_actions`/`uses_child`/`uses_list_module`
   repurpose into import toggles only.
7. **Regenerate** all goldens and the committed
   `examples/awl-hello/src/awl_hello.gleam` (deliberate re-baseline per the
   build-plan gate).

## 8. Test plan

- **SDK unit tests** (`gleam/aion_flow/test`, gleeunit): one test per §5
  table row; nested composites (`List(Option(Record))`); recursion through
  a `DRef` inside a list/option field; unicode strings; `DRef` miss →
  explicit failure; zero-field record round-trip; enum unknown-string
  failure text; union unknown-outcome failure text.
- **Contract round-trip per fixture**: construct values with the real
  generated constructors, `from(...)`-encode, decode, assert `==` — this is
  the test that anchors emitter `snake()` to the Gleam compiler's actual
  constructor atoms (§10 risk 2). Add a hostile-name fixture (consecutive
  capitals, digits) if the checker admits such type names.
- **Byte-parity capture**: before deleting the old generators, capture
  current goldens' encoded outputs for representative values as expected-
  bytes fixtures; the engine must reproduce them exactly.
- **Existing gates**: full `aion-awl`/`aion-cli` suites; the three compile
  proofs; awl-hello e2e trail unchanged; ≥40% line-count drop assertion.

## 9. Options evaluated

**(i) descriptor-full — CHOSEN.** Costs: one identity-coerce point (§4), a
6-function `.erl` shim, atom-table writes bounded by document size, and an
interpretive walk per boundary crossing (negligible against a durable-engine
dispatch; every crossing already does JSON string I/O through the FFI).
Buys: the entire codec surface leaves the emitted module; D4 has one
implementation; BC-3 loses its gnarliest template family (decoder
`use`-chains with closures) — record/enum/union/composite decode AND encode
all become "load literal, `call_ext`".

**(ii) descriptor-encode-only.** Honest appeal: encode consumes typed values,
so walking them needs only the safe coerce direction — no `from_dynamic`
anywhere. But it keeps per-type *decoders* generated, which is the more
expensive half for BC-3 (decoders are continuation-shaped `use`-chains with
per-field closures; encoders are flat expression trees), and it splits D4
into two implementations again (engine encode-omission vs generated
`optional_field` decode) — the exact re-derivation D-BC2 exists to kill. It
also still needs the shim's safe half plus record reflection. Half the risk
removed, well under half the benefit kept. Rejected.

**(iii) hoist-only (pre-authorized fallback).** Ships §6 minus
descriptor/engine; codec trios stay generated and become ~4 MIR template
shapes in BC-2/BC-3. Zero coerce risk, smallest BC-0, largest BC-3. This
remains the live fallback if panel review rejects §4 or implementation
fights Gleam beyond reasonable effort; the module map in §6 is deliberately
structured so the fallback is a strict subset (drop `descriptor.gleam`,
`codec.from`, `reflect`, the `.erl`; keep error/runtime/raw/decoded/
json_value hoists and the `result.try` switch).

## 10. Risks

1. **Coerce misuse by hand-written code.** `from` is public by necessity.
   Mitigation: contract doc comment, `aion/awl/*` documented as
   generated-code-reserved, `json_codec` stays the front door (§4.2 pt 5).
2. **Tag-snaking drift**: emitter `names::snake` vs the Gleam compiler's
   constructor-atom derivation could disagree on hostile names (consecutive
   capitals: `snake("HTTPStatus")` = `h_t_t_p_status`; Gleam's own choice
   must be verified). Today this cannot mis-encode (generated code names
   constructors syntactically); under descriptors it becomes load-bearing
   data. Mitigation: per-fixture round-trip contract tests + verify Gleam's
   algorithm during implementation + hostile-name fixture; worst case the
   emitter restricts/normalizes type-name shape at check time.
3. **Failure-path text drift** on decode errors would show in trails on
   failure paths. Mitigation: engine reuses `codec.json_codec` +
   `gleam/dynamic/decode` primitives (§3), so texts are same-source; the
   byte-parity capture tests pin it.
4. **`decode.recursive` availability** on the pinned stdlib floor
   (`>= 0.34`). Mitigation: the `decode.then(decode.dynamic, …)` lazy form
   needs nothing new; or bump the floor (aion_flow controls its manifest —
   the generated code already requires a modern `gleam/dynamic/decode`).
5. **Atom-table writes at decode time** (`binary_to_atom`). Bounded by the
   descriptor, which is bounded by the checked document; identical atoms to
   what module load creates anyway. Documented in the shim.
6. **Behavior widening by accident** (e.g. dropping the `zero_expr`
   refusal). Held closed deliberately in BC-0 (§3.3).
7. **BC-2/BC-3 literal expressibility** of descriptors: satisfied by
   construction (atoms/binaries/lists/tuples only), but BC-2's `LitT`
   encoder must round-trip a nested descriptor literal early — add one to
   the BC-1 round-trip corpus as soon as a generated module exists.

## 11. Open questions (none blocking implementation start)

1. Verify Gleam's exact constructor→atom derivation for consecutive
   uppercase/digit-bearing names; decide whether the checker should
   restrict type-name shape instead (risk 2).
2. `decode.recursive` vs the lazy-`then` form — pick during implementation
   based on the resolved stdlib version in `manifest.toml`.
3. Whether `aion/awl/codec.from` should take `#(Desc, Defs)` as one value to
   make the BC-3 literal a single `LitT` entry per boundary (cosmetic for
   Gleam, one fewer operand for bytecode; lean yes — decide at MIR design,
   BC-2).
4. Ticket text for the post-BC removal of the `zero_expr` recursive-required
   refusal (capability widening, needs its own tests).
5. Whether the retry loop later becomes an SDK combinator (out of BC-0
   scope; revisit after BC-3 shows its template cost).
