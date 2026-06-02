# Aion-Package — Checklist

## Crate Scaffold and Core Types

- [ ] **C1** — crates/aion-package exists, is registered as a workspace member, and depends only on aion-core among Aion crates (plus external crates: zip, serde, a hashing crate, thiserror).
- [ ] **C2** — src/lib.rs contains only pub mod declarations and pub use re-exports, no logic, and sets unsafe_code = "deny".
- [ ] **C3** — PackageError is a closed thiserror enum with distinct variants for: not-a-ZIP/archive-read failure, missing manifest, manifest parse failure, unknown format_version, missing entry module, integrity (hash) mismatch, and malformed beam entry.
- [ ] **C4** — BeamModule carries a logical module name and its exact compiled bytes; BeamSet is an ordered collection of BeamModules.
- [ ] **C5** — BeamSet exposes a canonical ordering (sorted by logical module name) that is deterministic and independent of insertion order.

## Manifest

- [ ] **C6** — Manifest is a typed struct carrying entry_module, entry_function, input_schema, output_schema, timeout, declared activities, version (content hash), and format_version.
- [ ] **C7** — Manifest derives Serialize and Deserialize and round-trips losslessly through serde_json against manifest.json.
- [ ] **C8** — format_version identifies the .aion format schema version and an unknown value is rejected at load with the unknown-format_version PackageError variant.
- [ ] **C9** — input_schema and output_schema hold JSON-Schema descriptions of the workflow input and result Payload shapes.
- [ ] **C10** — Declared activities enumerate the activity types the workflow invokes, each identifying its activity type.

## Content-Hash Versioning

- [ ] **C11** — The content hash is computed over the canonical beam set only (sorted logical names plus exact bytes), never over archive bytes, compression, timestamps, or source inclusion.
- [ ] **C12** — Two BeamSets with identical modules and bytes produce identical content hashes regardless of insertion order; any change to a beam's name or bytes changes the hash.
- [ ] **C13** — The content hash has a stable, documented textual form usable as a version identifier and as a module-name component.

## Module Namespacing

- [ ] **C14** — A namespacing function maps (logical module name, content hash) to a deployed module name (e.g. order_workflow$<hash>) deterministically.
- [ ] **C15** — An inverse function recovers the logical module name from a deployed module name; the transform is a verified round-trip (bijection).
- [ ] **C16** — Two packages with different content hashes produce disjoint deployed module-name sets, so distinct versions never collide in the registry.

## Write Path

- [ ] **C17** — PackageBuilder accepts a manifest (without the version), a BeamSet, and optional source, computes the content hash, and stamps it into the manifest as the version.
- [ ] **C18** — PackageBuilder writes a valid ZIP-container .aion with the fixed layout (manifest.json, beam/ entries, optional src/ entries) to a path or an in-memory buffer.
- [ ] **C19** — The write path is deterministic: identical inputs produce a byte-identical .aion, with no wall-clock timestamps or nondeterministic ordering in the archive.

## Read Path and Integrity

- [ ] **C20** — Package::load reads a .aion from a path and from an in-memory byte buffer, opening the ZIP and parsing manifest.json.
- [ ] **C21** — Load checks format_version and extracts the BeamSet (and optional source) from the archive entries.
- [ ] **C22** — Load recomputes the content hash over the extracted beams and rejects with the integrity-mismatch PackageError variant when it does not equal the manifest version.
- [ ] **C23** — Load is total: a truncated/non-ZIP input, a missing manifest, a missing entry module, and a malformed beam entry each map to their distinct PackageError variant with no panic and no partial result.
- [ ] **C24** — A loaded Package exposes the manifest, the BeamSet, the optional source, and the derived namespaced deployed module names ready for the engine to register.

## Version Record

- [ ] **C25** — A version record is producible from a loaded Package, carrying the logical entry module, the content hash, the declared activities, and the input/output schemas, using the shared aion-core version-record type.

## Round-Trip Conformance

- [ ] **C26** — A test suite round-trips PackageBuilder output through Package::load and asserts the manifest, beam set, and namespaced names survive intact.
- [ ] **C27** — A test asserts reproducibility (two builds of identical inputs are byte-identical) and asserts integrity rejection (a package whose beams are altered after packing fails to load).
