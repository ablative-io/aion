# Packaging Gleam workflows into `.aion` archives

This guide covers packaging any Gleam workflow project — not just the bundled examples — into the `.aion` archive format that `aion-server` loads. The tool is `aion-cli package`, a thin shell over the `aion_package::package_project` library function.

## Project layout

A packageable workflow project is an ordinary Gleam project with one extra file, `workflow.toml`, next to `gleam.toml`:

```text
my-workflow/
├── gleam.toml            # Gleam package metadata ([dependencies] matter here)
├── manifest.toml         # Gleam lockfile, written by the Gleam toolchain
├── workflow.toml         # Aion packaging descriptor (this guide)
├── schemas/
│   ├── input.json        # JSON Schema for workflow input
│   └── output.json       # JSON Schema for workflow output
├── src/
│   └── my_workflow.gleam # entry module exposing the entry function
└── build/                # produced by `gleam build`
```

The project must be **built** before packaging: the packager reads compiled BEAM files from `build/dev/erlang` and never invokes the compiler itself (the CLI's `--build` flag is the one exception, described below).

> **Naming note:** `workflow.toml` is the *packaging* descriptor. It is unrelated to the `aion.toml` file that `aion-server` auto-discovers in its working directory as *server* configuration — the two files configure different programs and never collide.

## `workflow.toml` reference

```toml
# Optional table. All fields below are optional.
[package]
include_source = true                    # default: true

# One or more workflow entries; each produces one .aion archive.
[[workflow]]
entry_module   = "my_workflow"           # REQUIRED
entry_function = "run"                   # REQUIRED
timeout_seconds = 3600                   # REQUIRED, integer >= 1
input_schema   = "schemas/input.json"    # REQUIRED, path relative to project root
output_schema  = "schemas/output.json"   # REQUIRED, path relative to project root
activities     = ["fetch", "store"]      # REQUIRED, may be []
output         = "my-workflow.aion"      # OPTIONAL, derived otherwise
```

### `[package]` (optional)

| Field | Type | Required | Meaning |
|---|---|---|---|
| `include_source` | bool | no (default `true`) | When `true`, every first-party source file (`src/**/*.gleam` of the root project, not dependencies) ships inside the archive under `src/`. Source inclusion **never changes the package version** — the content hash covers compiled modules only — so it costs archive bytes, not version identity. Set `false` for proprietary-source deployments. |

### `[[workflow]]` (one or more required)

| Field | Type | Required | Meaning |
|---|---|---|---|
| `entry_module` | string | **yes** | The compiled module whose function the engine calls. This is also the **workflow type** clients pass to `aion-cli start` — there is no separate type name. Must be a safe logical module name (no `$`, no path tricks) and unique across all `[[workflow]]` entries. Nested Gleam modules use `@` separators (`billing/cycle.gleam` compiles to `billing@cycle`). |
| `entry_function` | string | **yes** | The exported function on the entry module the engine invokes. The SDK convention is `run`, but it is a convention, not a default — state it explicitly. |
| `timeout_seconds` | integer | **yes** | Workflow execution timeout in whole seconds. Must be at least 1. |
| `input_schema` | string (path) | **yes** | Path to a JSON file, resolved against the project root, containing the JSON Schema for the workflow's input payload. The parsed document is embedded in the manifest verbatim; the file must exist and parse as JSON. |
| `output_schema` | string (path) | **yes** | Same as `input_schema`, for the workflow's result payload. |
| `activities` | array of strings | **yes** | The activity types the workflow schedules (the names workers register). Strings must be non-empty and unique. An explicitly empty list (`activities = []`) is legal for workflows that schedule no activities; *omitting* the field is an error — say "none" rather than forget it. |
| `output` | string (path) | no | Archive output path, resolved against the project root. When absent, derived as `<entry_module>.aion` in the project root. All `output` values (explicit or derived) must be pairwise distinct — checked on the normalized paths, so two spellings of the same file conflict. |

Unknown keys anywhere in the file are hard errors naming the key — typos fail loudly instead of being ignored.

**Root confinement (enforced).** Every path `workflow.toml` declares — `output`, `input_schema`, `output_schema` — must resolve inside the project root. Paths are normalized lexically (`.` and `..` folded, no filesystem access), then rejected with a typed error if they are absolute or escape the root; `sub/../out.aion` is fine, `../out.aion` is not. The one exception is the CLI's `--out` (the library's `output_override`): that path belongs to the *caller*, not the descriptor, and may point anywhere — including outside the project root.

### Multi-workflow projects

Each `[[workflow]]` entry produces its own `.aion` archive, and all archives from one project share the same beam set and therefore the same content-hash version. That is correct: a deployed workflow version is identified by (entry module, content hash), so deployed names like `parent$<hash>` and `child$<hash>` remain distinct.

## What goes into the archive

Discovery starts from the compiled tree `build/dev/erlang`:

1. **Production dependency closure.** Only packages in the production closure are included: the root package plus the transitive closure of `gleam.toml`'s `[dependencies]` resolved through the `manifest.toml` lockfile. Dev-dependencies of the root project (test frameworks such as `gleeunit`) are discovered but **excluded**, and each exclusion is reported (`reason: "dev_dependency"`).
2. **Per-package modules.** Every `<package>/ebin/*.beam` file becomes a module; the logical module name is the file stem. Non-`.beam` entries (`.app` files, `fingerprint` directories) are skipped.
3. **SDK test-module filter.** The `aion_flow` SDK's own ebin ships test machinery that must never deploy: `aion_flow_ffi` (the in-process engine double, which occupies the engine-owned NIF namespace) and the `aion@testing` / `aion@testing@*` modules. These are excluded *only when found in the `aion_flow` package* and each exclusion is reported (`reason: "sdk_test_only"`). A module of your own named `aion_flow_ffi` is **not** silently dropped — it fails packaging with a reserved-module-name error.
4. **First-party source** (unless `include_source = false`): all `src/**/*.gleam` files of the root project, stored under `src/` keyed by module path.

Every written archive is **re-loaded and re-validated before the tool reports success** — integrity hash, format version, and entry-module presence are all proven on the read path, so a reported package is a loadable package.

## Determinism and versioning

For a fixed built tree and a fixed `workflow.toml`, packaging is byte-for-byte reproducible: modules are stored in canonical name order with fixed timestamps and permissions, and repeated runs produce identical archives.

The package **version** is the SHA-256 content hash of the compiled modules (logical names + exact bytes, in canonical order). It changes when, and only when, compiled code changes:

- Recompiling without source changes → same version.
- Toggling `include_source` → different archive bytes, **same version**.
- Editing manifest fields (timeout, schemas, activities) → same version (the manifest describes the code; it is not the code).

The server deploys each version as immutable modules named `<module>$<hash>`, which is how long-running workflows keep executing old code while new versions deploy alongside.

## CLI usage

```sh
aion-cli package [PATH] [--out <FILE>] [--build] [--pretty]
```

- `PATH` — workflow project root (the directory containing `workflow.toml`). Defaults to the current directory.
- `--out <FILE>` — write the archive to this path instead of the configured/derived output. Resolved against your current directory. Only valid when the project declares exactly one workflow. As the caller's own path it is exempt from the descriptor's root confinement and may point anywhere.
- `--build` — run `gleam build` in the project first (compiler output appears on stderr). Without it, packaging an unbuilt project fails with an error telling you to build.
- `--pretty` — pretty-print the JSON result document (global flag).

`package` runs entirely locally and never connects to a server, so `--endpoint`/`--namespace`/`--subject` are irrelevant to it.

On success it prints one JSON document to stdout and exits 0:

```json
{
  "packages": [
    {
      "workflow_type": "hello_world",
      "output": "/abs/path/examples/hello-world/hello-world.aion",
      "version": "ecc4510a5c16641d9f93c95420cbca302c9f428321e41b84c9db9692a6af881a",
      "deployed_name": "hello_world$ecc4510a5c16641d9f93c95420cbca302c9f428321e41b84c9db9692a6af881a",
      "modules": 42
    }
  ],
  "excluded": [
    { "module": "aion_flow_ffi", "package": "aion_flow", "reason": "sdk_test_only" }
  ]
}
```

`packages[].workflow_type` is the name to pass to `aion-cli start`; `packages[].output` is the path to load with `aion-server --workflow-package` or a `workflow_packages` config entry — or to deploy into a *running* server with `aion-cli deploy <archive>` when the server's `[deploy]` surface is enabled (see [docs/API.md — Operator deploy API](API.md#operator-deploy-api)). The document is `jq`-friendly: `jq -r '.packages[0].output'` extracts the archive path for deploy scripts. Errors print an `Error:` chain on stderr and exit 1; CLI usage mistakes exit 2.

## Troubleshooting

Errors name the offending file, field, or module. Common ones:

| Error | Cause / fix |
|---|---|
| `no workflow.toml found in <root>` | The path you packaged is not a workflow project root, or the descriptor has not been written yet. |
| `failed to parse <path>: ...` | TOML syntax error, a missing required field, or an unknown key in `workflow.toml`; the message names it. |
| `invalid workflow.toml: <field>: <reason>` | A semantic rule failed: zero timeout, empty/unsafe entry module, duplicate entry modules or activities, empty `activities` strings, no `[[workflow]]` entries. |
| `failed to read schema <path>` / `schema <path> is not valid JSON` | A declared schema file is missing or malformed. Paths resolve against the project root. |
| `project is not built: <path> does not exist; run gleam build first` | Run `gleam build` in the project, or pass `--build`. |
| `not a Gleam project: <path> not found` / `failed to read Gleam metadata <path>` | No `gleam.toml`/`manifest.toml` where expected; packaging must point at a Gleam project root. |
| `dependency <package> is in gleam.toml but missing from manifest.toml; rebuild` | The lockfile is stale relative to `gleam.toml`; rebuild so Gleam regenerates it. |
| `module <m> is provided by both <a> and <b>` | Two packages compile a module with the same logical name; rename one. |
| `entry module <m> not found in compiled output under <path>` | The `entry_module` in `workflow.toml` does not match a compiled module — check spelling and remember nested modules use `@` (`a/b.gleam` → `a@b`). |
| `invalid workflow.toml: <field>: path <path> is absolute or escapes the project root` | A declared `output`, `input_schema`, or `output_schema` path is absolute or `..`-traverses above the project root. Descriptor paths are confined to the root; use a relative in-root path (or `--out` for caller-chosen output locations). |
| `workflows <a> and <b> both write to <path>` | Two `[[workflow]]` entries resolve to the same output file (after normalization, so differently-spelled paths to one file count); set distinct `output` values. |
| `--out is only valid for single-workflow projects` | Drop `--out` or use per-workflow `output` fields. |
| `failed to write archive <path>: ...` | The named output path could not be written — most often its parent directory does not exist; create it first. |
| `module <m> uses an engine-reserved namespace and must not ship as package bytecode` | Your project defines a module in the engine-owned namespace (e.g. `aion_flow_ffi`); rename it. |

## Library usage (embedders)

The CLI is a thin shell. Programs that want packaging as a function link `aion-package` directly:

```rust
use aion_package::{package_project, PackageOptions};

let options = PackageOptions::default(); // construct via Default, then assign fields
let report = package_project(project_root, &options)?;
for packaged in &report.packages {
    println!("{} -> {}", packaged.workflow_type, packaged.output_path.display());
}
```

`package_project` is pure with respect to the environment: it reads only under the project root, writes only the declared outputs, never spawns processes, reads no environment variables, and never prints — everything observable is in the returned `ProjectReport` or the structured `PackagingError`. The read/write confinement is enforced, not assumed: `workflow.toml`-declared paths that are absolute or escape the root fail with `PackagingError::PathEscapesRoot` before any file is touched, with `PackageOptions::output_override` as the sole, caller-owned exception. It performs blocking filesystem I/O; async callers should wrap it in a blocking task (e.g. `tokio::task::spawn_blocking`).
