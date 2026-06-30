# aion-dashboard

Standalone Vite SPA for human-facing Aion server observability.

## Two ways to serve it

* **Dev (live, hot reload):** `bun install && bun run dev` (defaults to
  `http://localhost:5173`). The dev server talks to a running `aion server` over
  the network using `VITE_*` config and **CORS** — set the server's
  `cors_allowed_origins` to include the dev origin. This is the loop you use
  while building the UI.
* **Shipped (embedded in the single binary):** the built bundle is ALWAYS
  compiled into the `aion` binary and served at the server's **HTTP port** (same
  port as the API). No separate web server, and no cargo feature — a plain
  `cargo build` ships the real UI.

### Embed pipeline

The ops console is part of the product: `crates/aion-server/dashboard-embed/` is
a **committed** bundle and every build embeds it. You only run the pipeline to
**refresh** that committed bundle after changing the dashboard source:

```sh
cargo xtask build-dashboard
```

In order it: (a) regenerates the ts-rs wire types
(`cargo test -p aion-core export_dashboard_wire_types`), (b) runs
`bun install && bun run build` here, and (c) syncs `dist/*` into
`crates/aion-server/dashboard-embed/`. **Commit the refreshed bundle.**

Then any build ships it — no feature flag:

```sh
cargo build -p aion-cli --release
./target/release/aion server --open   # serves the dashboard at the HTTP port
```

**The bundle is committed.** `crates/aion-server/dashboard-embed/` holds the real
built `index.html`, `assets/**`, and `favicon.svg`. `rust_embed` reads it from
disk in debug builds and embeds it in release builds. The Vite config builds with
`sourcemap: false`, and the embed dir's `.gitignore` ignores stray `*.map` files.

### CI guards (`.github/workflows/dashboard-embed.yml`)

Two distinct guards (kept separate so there is no write-then-assert
contradiction):

1. **wire-types-no-diff** — regenerate the ts-rs types and assert the committed
   generated files have no git diff (read-only assertion).
2. **embed-freshness** — `cargo xtask verify-dashboard` rebuilds the dashboard
   into a scratch dir and diffs it against the committed `dashboard-embed/`
   bundle, failing if it is stale. It then builds a plain (no-feature) binary and
   `ci/smoke-embed.sh` runs it, failing if `/` does not serve the real UI.

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
