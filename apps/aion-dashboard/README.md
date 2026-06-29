# aion-dashboard

Standalone Vite SPA for human-facing Aion server observability.

## Two ways to serve it

* **Dev (live, hot reload):** `bun install && bun run dev` (defaults to
  `http://localhost:5173`). The dev server talks to a running `aion server` over
  the network using `VITE_*` config and **CORS** — set the server's
  `cors_allowed_origins` to include the dev origin. This is the loop you use
  while building the UI.
* **Shipped (embedded in the single binary):** the built bundle is compiled into
  the `aion` binary and served at the server's **HTTP port** (same port as the
  API). No separate web server.

### Embed pipeline (WS5)

One command builds the bundle and stages it for the embed build:

```sh
cargo xtask build-dashboard
```

In order it: (a) regenerates the ts-rs wire types
(`cargo test -p aion-core export_dashboard_wire_types`), (b) runs
`bun install && bun run build` here, and (c) syncs `dist/*` into
`crates/aion-server/dashboard-embed/`.

Then build the server (or CLI) with the embed feature on:

```sh
cargo build -p aion-server --features embed-dashboard
# or the release binary, which turns the feature on for you:
cargo build -p aion-cli --release --features release
./target/release/aion server --open   # serves the dashboard at the HTTP port
```

**Feature off (default).** A plain `cargo build` does NOT require bun and does
NOT embed the bundle: backend-only devs build with no JS toolchain. With the
feature off, `/` serves a branded placeholder page documenting the dev URL and
the build command — never a blank page or a silent stub.

**Built assets are gitignored.** `crates/aion-server/dashboard-embed/` keeps only
a committed placeholder `index.html` (the `AION_EMBED_PLACEHOLDER` stub) so the
`rust_embed` folder compiles on a fresh checkout. CI/release runs
`cargo xtask build-dashboard` before the embed build, which overwrites that stub
with the real bundle.

### CI guards (`.github/workflows/dashboard-embed.yml`)

Three distinct guards (kept separate so there is no write-then-assert
contradiction):

1. **wire-types-no-diff** — regenerate the ts-rs types and assert the committed
   generated files have no git diff (read-only assertion).
2. **embed-build** — run the pipeline, then build with `--features embed-dashboard`.
3. **release-feature guard** — the release build always passes `--features release`,
   and `ci/smoke-embed.sh` runs the binary and fails if `/` serves the
   placeholder stub (catches a release that forgot the pipeline/feature).

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
