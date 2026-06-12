# aion-package

Archive validation, content hashing, and namespacing for Aion workflow packages. The crate loads `.aion` archives, validates their manifest and compiled BEAM entries, computes stable content hashes, and derives deployed module names for engine registration.

## Install

```toml
[dependencies]
aion-package = "0.4.0"
```

## Key public types

- `Package` represents a validated in-memory `.aion` archive.
- `PackageBuilder` assembles package archives for tests and tooling.
- `Manifest`, `ManifestVersion`, and `DeclaredActivity` describe archive metadata.
- `BeamModule`, `BeamSet`, `ContentHash`, and `WorkflowVersion` identify package contents.
- `deployed_name`, `deployed_names`, and `ParsedDeployedName` handle module namespacing.

## Minimal usage

```rust
use aion_package::Package;

let package = Package::load_from_path("workflow.aion")?;
println!("entry module: {}", package.manifest().entry_module);
# Ok::<(), Box<dyn std::error::Error>>(())
```
