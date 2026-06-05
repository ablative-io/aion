# aion-dashboard

Standalone Vite SPA for human-facing `aion-server` observability.

## Generated wire types

Wire types under `src/types/generated/` are generated from Rust-owned Aion types with the
house generated-types pipeline (`#[derive(TS)]` via `ts-rs`). Do not edit generated files by hand.
The generated barrel carries a `@generated` header so review can distinguish wire output from local
view models.

Regenerate after changing exported Aion wire shapes:

```sh
cargo test -p aion-core export_dashboard_wire_types
```

The repository-level `just gen-types` task should call the same ts-rs export path when the house
justfile is present.

The generation target for this app is `apps/aion-dashboard/src/types/generated/`. The public
`src/types/index.ts` file is only a barrel over generated wire types; local UI-only models belong in
feature-folder `types.ts` files.
