# aion-nif

Native function declaration helpers for Gleam and Elixir Aion workflows. The crate provides typed conversions between BEAM terms and Aion payloads, deterministic and activity NIF descriptors, registry builders, and suspension helpers used by workflow runtimes.

## Install

```toml
[dependencies]
aion-nif = "0.1.0"
```

## Key public types

- `Nif`, `Determinism`, `NifSet`, and `NifSetBuilder` describe and collect native functions.
- `NifContext` wraps the BEAM process context used during conversions.
- `FromTerm`, `IntoTerm`, `AtomName`, and payload helpers bridge typed Rust values and BEAM terms.
- `deterministic_nif!`, `activity_nif!`, and `ActivityWakeHandle` support declaration and suspension.
- `NifDeclError` and `TermError` report declaration and conversion failures.

## Minimal usage

```rust
use aion_nif::{NifSet, deterministic_nif};

fn double(value: i64) -> i64 {
    value * 2
}

let nifs = NifSet::builder()
    .register(deterministic_nif!("math", "double", double, (value: i64) -> i64))?
    .build();
assert_eq!(nifs.len(), 1);
# Ok::<(), Box<dyn std::error::Error>>(())
```
