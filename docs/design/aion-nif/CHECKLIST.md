# Aion-Nif — Checklist

## Crate Scaffold and Error Taxonomy

- [ ] **C1** — crates/aion-nif exists as a workspace member; Cargo.toml depends on beamr and aion-core only, with no dependency on the aion engine crate.
- [ ] **C2** — src/lib.rs contains only module declarations and re-exports, no logic.
- [ ] **C3** — The crate sets unsafe_code = "deny" at the root and inherits the workspace clippy lints.
- [ ] **C4** — TermError is a typed error enum (thiserror) describing a term-conversion failure, including the failing argument index where applicable, not a bare badarg.
- [ ] **C5** — NifDeclError is a typed error enum (thiserror) describing declaration/registration-set failures, including duplicate (module, function, arity).

## Term Conversion

- [ ] **C6** — FromTerm decodes a Term (with ProcessContext for atom resolution) into a Rust type, returning TermError on type mismatch.
- [ ] **C7** — IntoTerm encodes a Rust value into a Term, allocating on the process heap through the ProcessContext, returning TermError on allocation failure.
- [ ] **C8** — FromTerm and IntoTerm are implemented for signed and unsigned integers, floats, and booleans.
- [ ] **C9** — FromTerm and IntoTerm are implemented for binaries (Vec<u8> and String) and for atoms via a dedicated atom-name newtype.
- [ ] **C10** — FromTerm and IntoTerm are implemented for Option<T> (atom nil for None, the value for Some) and for a Result-shaped {ok, T} / {error, E} tuple.
- [ ] **C11** — FromTerm and IntoTerm are implemented for homogeneous lists (Vec<T>) over proper cons lists, and for string-keyed maps (BTreeMap<String, T>).
- [ ] **C12** — A round-trip test (encode then decode) holds for every implemented type across representative values.

## Payload and JSON Bridge

- [ ] **C13** — Payload (aion-core) converts to and from a Term via the JSON content-type (term_to_value to serde_json::Value to Payload and the reverse), round-tripping losslessly.
- [ ] **C14** — A blanket FromTerm/IntoTerm is provided for any T: Serialize + DeserializeOwned, routed through Payload, so a NIF can be written over ordinary Rust structs.
- [ ] **C15** — The payload module is the only place aion-nif references aion-core's Payload and ActivityError.

## NIF Descriptor and Determinism Tag

- [ ] **C16** — Determinism is an enum with exactly Pure and SideEffectful variants.
- [ ] **C17** — The Nif descriptor records module name, function name, arity, the NativeFn shim, a dirty-scheduler flag, and a Determinism tag.
- [ ] **C18** — The engine's registration path can read a Nif's Determinism tag to choose binding mode (inline for Pure, activity-body for SideEffectful).

## Deterministic Helper Declaration

- [ ] **C19** — deterministic_nif takes a Rust fn/closure with FromTerm arguments and an IntoTerm return and produces a Nif tagged Determinism::Pure.
- [ ] **C20** — The generated shim checks arity, decodes each argument by position (reporting the failing index on mismatch), calls the body, and encodes the return — author code never sees raw &[Term].
- [ ] **C21** — The generated return encoding is heap-safe (uses the ProcessContext allocation path) and does not Box::leak in any author-reachable path.
- [ ] **C22** — A panic raised inside the author body is caught and converted to a typed NIF error rather than unwinding across the FFI boundary.

## Side-Effectful Activity Declaration

- [ ] **C23** — activity_nif produces a Nif tagged Determinism::SideEffectful with the dirty-scheduler flag set, whose body returns Result<T, ActivityError>.
- [ ] **C24** — There is no untyped declaration path: deterministic_nif yields only Pure and activity_nif yields only SideEffectful, so a side-effectful NIF cannot be expressed as an inline deterministic helper.
- [ ] **C25** — An ActivityError returned by the body is encoded so its retryable/terminal classification survives the boundary back to the engine.
- [ ] **C26** — Where suspension is needed, the crate exposes beamr's existing request_suspend / wake_with_result mechanism rather than introducing a new async model.

## Registration and FFI Seam

- [ ] **C27** — NifSet collects declared Nifs under a module name and produces the value EngineBuilder::register_nifs consumes.
- [ ] **C28** — NifSet rejects a duplicate (module, function, arity) at build time with a typed NifDeclError and never silently overwrites.
- [ ] **C29** — aion-nif does not call beamr's BifRegistryImpl::register itself; the descriptor set is registered by the engine's runtime module.
- [ ] **C30** — Any unsafe is confined to the raw module behind safe wrappers, each unsafe block carrying a // SAFETY: comment; no unsafe appears outside raw.
- [ ] **C31** — A minimal illustrative NIF set (at least one Pure helper and one SideEffectful activity) exercises the full declaration-to-descriptor path and asserts the determinism tags and dispatch shapes.
