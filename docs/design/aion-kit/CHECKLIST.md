# Aion-Kit — Checklist

## Package Skeleton

- [ ] **C1** — gleam/aion_kit/gleam.toml exists naming the package aion_kit, target erlang, with dependencies gleam_stdlib, gleam_json, and aion_flow, and dev-dependency gleeunit, matching the gleam/aion_flow manifest shape (licences array, github repository).
- [ ] **C2** — gleam/aion_kit/src/aion_kit.gleam is a declaration-only package-root module (a PackageRoot marker type and module docs only, no functions), matching the aion_flow/aion_client root convention.
- [ ] **C3** — cd gleam/aion_kit && gleam build and gleam test both exit 0 against the aion_flow dependency, and gleam format --check reports no changes.

## Opaque Payload

- [ ] **C4** — aion_flow exposes a Sealed pass-through type in src/aion/payload.gleam carrying a content-type tag and the raw payload bytes as a String, with no accessor that returns a decoded structured view of the bytes.
- [ ] **C5** — A workflow can hold a Sealed and forward it as the next activity's input without decoding: a test seals a value, reads only its raw bytes, and decodes the value only with an explicit decoder on the far side — the Sealed type itself never materialises the decoded structure.
- [ ] **C6** — aion_kit/payload.seal turns a typed value into a Sealed via a caller-supplied json encoder and an explicit content-type, inventing no default content-type.
- [ ] **C7** — aion_kit/payload.raw returns the Sealed's opaque bytes (and content_type accessor returns the tag) so a consuming activity can decode them with its own typed decoder.
- [ ] **C8** — aion_kit/payload.peek decodes ONLY a caller-supplied thin facts view from a Sealed, ignoring every field the view does not name, and returns a typed decode error (never a panic) when the bytes do not satisfy the thin decoder; a test peeks a small facts view out of a sealed report far larger than the view.

## Template

- [ ] **C9** — aion_kit/template.join_blocks joins sections with a blank line between them and drops empty-string sections; join_lines joins with single newlines and drops empty-string lines — reproducing the prompts.gleam behaviour exactly.
- [ ] **C10** — aion_kit/template.bulleted renders each item as a '- item' line joined by newlines, and kv renders a single 'key: value' line.
- [ ] **C11** — aion_kit/template.render substitutes every '{name}' placeholder in a template string from a list of (name, value) bindings, leaving text with no placeholder unchanged and substituting every occurrence of a bound name.
- [ ] **C12** — render's behaviour for a placeholder with no matching binding is explicit and tested (the unmatched placeholder is left verbatim, never replaced with an invented empty or default value), per ADR-001.

## JSON Wrangling

- [ ] **C13** — aion_kit/json.project decodes a thin typed view from a JSON string using a caller-supplied decoder and succeeds even when the JSON carries fields the view does not name (the ignored fields are dropped, not an error).
- [ ] **C14** — aion_kit/json.merge deep-merges two JSON values append-only and order-stable: objects merge key-by-key with the right value winning a leaf conflict, arrays concatenate left-then-right, and existing key insertion order is preserved.
- [ ] **C15** — aion_kit/json.pluck reads the value at a path (a list of object keys) from a JSON value and returns it, or a typed miss when any key in the path is absent or a non-object is traversed — never a panic and never a guessed default.
- [ ] **C16** — Each of project, merge, and pluck has a negative-case test: a field absent from the projection still decodes, a leaf conflict in merge resolves right-wins, and a missing pluck path returns the typed miss.

## Purity and Boundary

- [ ] **C17** — No file in gleam/aion_kit/src nor gleam/aion_flow/src/aion/payload.gleam imports aion/workflow, aion/activity, aion/query, aion/signal, references the norn agent driver, or spawns any process/port — grep for those imports and for 'norn' over the package finds no matches (CN1, ADR-011).
- [ ] **C18** — No public function in aion_kit or the Sealed accessors reads a wall clock, draws entropy, or performs IO; library code contains no panic, assert, or let-assert — decode and lookup failures are Result/Option values (CN2, CN7).
