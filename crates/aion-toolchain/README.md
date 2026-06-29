# aion-toolchain

Server-side Gleam authoring toolchain for [Aion](https://github.com/ablative-io/aion) workflows. It shells out to the external `gleam` binary to compile and type-check workflow source, then packages a verified `.aion` archive. The crate embeds **no** compiler — it only drives the `gleam` toolchain you already have installed.

## Install

```toml
[dependencies]
aion-toolchain = "0.8.0"
```

## Key public types

- `Workspace` resolves and manages an on-disk Gleam project workspace.
- `CompileRequest` describes a compilation request (source, project, options).
- `CompiledWorkflow` is the verified result of a successful build.
- `compile_source` and `build_project` run the `gleam` toolchain to type-check and compile.
- `ToolchainError` is the error type returned across the API.

## Notes

The `gleam` binary must be on `PATH` (or supplied via the caller's configuration). Authoring endpoints in `aion-server` stay dark unless a Gleam path is configured, so depending on this crate adds no compiler to a server build.
